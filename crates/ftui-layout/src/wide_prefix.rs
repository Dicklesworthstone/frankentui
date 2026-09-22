//! Checked wide-domain prefix sums for virtualized content.
//!
//! [`WidePrefixSums`] is the dependency-light arithmetic seam for hosts whose
//! logical content is larger than a terminal coordinate or a `u32` total.
//! It deliberately owns no terminal, runtime, renderer, or persistence
//! policy. Callers can use it for reader heights, virtualized blocks, or other
//! non-terminal sequences of non-negative `u64` values.
//!
//! The structure is a Fenwick tree. Updates and queries are logarithmic, while
//! all arithmetic is checked and failed updates leave the existing tree
//! unchanged. `partition_point` returns the number of complete entries whose
//! prefix sum fits within an offset, which avoids the ambiguous inclusive
//! indexing of the original terminal-oriented helper.

/// A checked Fenwick tree over non-negative `u64` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WidePrefixSums {
    /// One-indexed Fenwick storage; slot zero is intentionally unused.
    tree: Vec<u64>,
}

impl WidePrefixSums {
    /// Create an all-zero tree with `len` entries.
    ///
    /// The allocation is fallible and the `len + 1` slot calculation is
    /// checked before attempting it.
    pub fn try_new(len: usize) -> Result<Self, WidePrefixError> {
        let slots = len
            .checked_add(1)
            .ok_or(WidePrefixError::CapacityOverflow { len })?;
        let mut tree = Vec::new();
        tree.try_reserve_exact(slots)
            .map_err(|_| WidePrefixError::AllocationFailed { len })?;
        tree.resize(slots, 0);
        Ok(Self { tree })
    }

    /// Build a tree from values in linear time.
    ///
    /// Fails with `SumOverflow` at the first entry whose inclusive prefix
    /// exceeds `u64::MAX`.
    pub fn try_from_values(values: &[u64]) -> Result<Self, WidePrefixError> {
        // Every prefix and every tree node is a sum of entries, so a total
        // that fits bounds them all. Checking only the nodes admitted
        // `[u64::MAX, 0, 1]`: no node overflows, but its total does, and
        // then `total`, `value(2)`, `set` and `remove` all failed on it.
        let mut total = 0_u64;
        for (index, &value) in values.iter().enumerate() {
            total = total
                .checked_add(value)
                .ok_or(WidePrefixError::SumOverflow { index })?;
        }
        let mut result = Self::try_new(values.len())?;
        for (index, &value) in values.iter().enumerate() {
            result.tree[index + 1] = value;
        }
        for index in 1..=values.len() {
            let Some(parent) = index.checked_add(lowbit(index)) else {
                continue;
            };
            if parent <= values.len() {
                result.tree[parent] += result.tree[index];
            }
        }
        Ok(result)
    }

    /// Number of entries in the sequence.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tree.len() - 1
    }

    /// Whether the sequence has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Return the value at `index`.
    pub fn value(&self, index: usize) -> Result<u64, WidePrefixError> {
        self.check_index(index)?;
        let end = self.prefix_before(index + 1)?;
        let start = self.prefix_before(index)?;
        end.checked_sub(start)
            .ok_or(WidePrefixError::InvariantViolation { index })
    }

    /// Return the sum of entries in `[0, end)`.
    ///
    /// `end == len()` is valid and returns the total.
    pub fn prefix_before(&self, end: usize) -> Result<u64, WidePrefixError> {
        if end > self.len() {
            return Err(WidePrefixError::EndOutOfBounds {
                end,
                len: self.len(),
            });
        }
        let mut index = end;
        let mut sum = 0_u64;
        while index != 0 {
            sum = sum
                .checked_add(self.tree[index])
                .ok_or(WidePrefixError::SumOverflow { index: index - 1 })?;
            index -= lowbit(index);
        }
        Ok(sum)
    }

    /// Return the inclusive prefix ending at `index`.
    pub fn prefix_inclusive(&self, index: usize) -> Result<u64, WidePrefixError> {
        self.check_index(index)?;
        self.prefix_before(index + 1)
    }

    /// Return the sum of entries in `[start, end)`.
    pub fn range(&self, start: usize, end: usize) -> Result<u64, WidePrefixError> {
        if start > end || end > self.len() {
            return Err(WidePrefixError::InvalidRange {
                start,
                end,
                len: self.len(),
            });
        }
        let before_start = self.prefix_before(start)?;
        let before_end = self.prefix_before(end)?;
        before_end
            .checked_sub(before_start)
            .ok_or(WidePrefixError::InvariantViolation { index: start })
    }

    /// Return the total sum of all entries.
    pub fn total(&self) -> Result<u64, WidePrefixError> {
        self.prefix_before(self.len())
    }

    /// Add a signed adjustment to one entry.
    ///
    /// The entry and the total are checked before any mutation, so taking
    /// the entry below zero or the total above `u64::MAX` fails and leaves
    /// the tree unchanged.
    pub fn add(&mut self, index: usize, delta: i64) -> Result<(), WidePrefixError> {
        self.check_index(index)?;
        if delta >= 0 {
            self.apply_unsigned_delta(index, delta as u64, true)
        } else {
            self.apply_unsigned_delta(index, delta.unsigned_abs(), false)
        }
    }

    /// Set one entry to a new non-negative value.
    pub fn set(&mut self, index: usize, value: u64) -> Result<(), WidePrefixError> {
        let current = self.value(index)?;
        if value >= current {
            self.apply_unsigned_delta(index, value - current, true)
        } else {
            self.apply_unsigned_delta(index, current - value, false)
        }
    }

    /// Rebuild the tree from a new sequence, preserving the old tree if the
    /// new sequence cannot be represented or allocated.
    pub fn rebuild(&mut self, values: &[u64]) -> Result<(), WidePrefixError> {
        let replacement = Self::try_from_values(values)?;
        *self = replacement;
        Ok(())
    }

    /// Insert one entry and rebuild the bounded prefix directory.
    ///
    /// Structural edits are intentionally explicit and linear-time. The
    /// replacement tree is fully validated before it becomes visible, so a
    /// failed allocation or sum check leaves this tree unchanged.
    pub fn insert(&mut self, index: usize, value: u64) -> Result<(), WidePrefixError> {
        let new_len = self
            .len()
            .checked_add(1)
            .ok_or(WidePrefixError::CapacityOverflow { len: self.len() })?;
        if index > self.len() {
            return Err(WidePrefixError::IndexOutOfBounds {
                index,
                len: self.len(),
            });
        }
        let mut values = self.snapshot_values()?;
        values
            .try_reserve_exact(1)
            .map_err(|_| WidePrefixError::AllocationFailed { len: new_len })?;
        values.insert(index, value);
        let replacement = Self::try_from_values(&values)?;
        *self = replacement;
        Ok(())
    }

    /// Remove one entry and rebuild the bounded prefix directory.
    pub fn remove(&mut self, index: usize) -> Result<u64, WidePrefixError> {
        self.check_index(index)?;
        let mut values = self.snapshot_values()?;
        let removed = values.remove(index);
        let replacement = Self::try_from_values(&values)?;
        *self = replacement;
        Ok(removed)
    }

    /// Return the number of complete entries whose prefix sum is at most
    /// `offset`.
    ///
    /// The result is in `0..=len()`. Zero-height entries are included, making
    /// the result suitable for selecting a virtualized row at an exact
    /// boundary without an inclusive/exclusive ambiguity.
    #[must_use]
    pub fn partition_point(&self, offset: u64) -> usize {
        let mut position = 0_usize;
        let mut consumed = 0_u64;
        let mut bit = highest_power_of_two(self.len());
        while bit != 0 {
            let Some(next) = position.checked_add(bit) else {
                bit >>= 1;
                continue;
            };
            if next <= self.len()
                && let Some(candidate) = consumed.checked_add(self.tree[next])
                && candidate <= offset
            {
                position = next;
                consumed = candidate;
            }
            bit >>= 1;
        }
        position
    }

    fn check_index(&self, index: usize) -> Result<(), WidePrefixError> {
        if index >= self.len() {
            return Err(WidePrefixError::IndexOutOfBounds {
                index,
                len: self.len(),
            });
        }
        Ok(())
    }

    fn snapshot_values(&self) -> Result<Vec<u64>, WidePrefixError> {
        let mut values = Vec::new();
        values
            .try_reserve_exact(self.len())
            .map_err(|_| WidePrefixError::AllocationFailed { len: self.len() })?;
        for index in 0..self.len() {
            values.push(self.value(index)?);
        }
        Ok(values)
    }

    fn apply_unsigned_delta(
        &mut self,
        index: usize,
        delta: u64,
        increasing: bool,
    ) -> Result<(), WidePrefixError> {
        // Each node on the update path is a sum of entries that includes this
        // one, so it lies between the entry and the total. Keeping those two
        // in range keeps every node in range. Checking the nodes alone missed
        // both: on `[5, 5]`, `add(1, -7)` found node 2 (holding 10) able to
        // absorb it and left entry 1 at -2, and on `[u64::MAX, 0, 0]`,
        // `add(2, 1)` found no node overflowing and pushed the total past
        // `u64::MAX`.
        if increasing {
            self.total()?
                .checked_add(delta)
                .ok_or(WidePrefixError::SumOverflow { index })?;
        } else if self.value(index)? < delta {
            return Err(WidePrefixError::SumUnderflow { index });
        }

        let mut cursor = index + 1;
        while cursor <= self.len() {
            self.tree[cursor] = if increasing {
                self.tree[cursor] + delta
            } else {
                self.tree[cursor] - delta
            };
            // A missing next index is past any representable length, so
            // this was the last node on the path.
            let Some(next) = cursor.checked_add(lowbit(cursor)) else {
                break;
            };
            cursor = next;
        }
        Ok(())
    }
}

/// Failure returned by checked wide-prefix operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidePrefixError {
    /// `len + 1` cannot be represented as a `usize`.
    CapacityOverflow { len: usize },
    /// The allocator rejected the requested capacity.
    AllocationFailed { len: usize },
    /// An entry index is outside the sequence.
    IndexOutOfBounds { index: usize, len: usize },
    /// A half-open range or endpoint is invalid.
    InvalidRange {
        start: usize,
        end: usize,
        len: usize,
    },
    /// A prefix endpoint is larger than the sequence length.
    EndOutOfBounds { end: usize, len: usize },
    /// A checked prefix or update would exceed `u64::MAX`.
    SumOverflow { index: usize },
    /// A checked decrement would make a prefix negative.
    SumUnderflow { index: usize },
    /// Internal prefix ordering was inconsistent.
    InvariantViolation { index: usize },
}

impl std::fmt::Display for WidePrefixError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CapacityOverflow { len } => {
                write!(formatter, "wide-prefix length overflow: {len}")
            }
            Self::AllocationFailed { len } => {
                write!(formatter, "wide-prefix allocation failed: {len}")
            }
            Self::IndexOutOfBounds { index, len } => {
                write!(
                    formatter,
                    "wide-prefix index {index} out of bounds (len={len})"
                )
            }
            Self::InvalidRange { start, end, len } => {
                write!(
                    formatter,
                    "wide-prefix range [{start}, {end}) invalid (len={len})"
                )
            }
            Self::EndOutOfBounds { end, len } => {
                write!(formatter, "wide-prefix end {end} out of bounds (len={len})")
            }
            Self::SumOverflow { index } => write!(formatter, "wide-prefix sum overflow at {index}"),
            Self::SumUnderflow { index } => {
                write!(formatter, "wide-prefix sum underflow at {index}")
            }
            Self::InvariantViolation { index } => {
                write!(formatter, "wide-prefix invariant violation at {index}")
            }
        }
    }
}

impl std::error::Error for WidePrefixError {}

#[inline]
fn lowbit(value: usize) -> usize {
    value.isolate_lowest_one()
}

#[inline]
fn highest_power_of_two(value: usize) -> usize {
    if value == 0 {
        0
    } else {
        1 << (usize::BITS - 1 - value.leading_zeros())
    }
}

#[cfg(test)]
mod tests {
    use super::{WidePrefixError, WidePrefixSums};

    #[test]
    fn supports_totals_above_u32() {
        let sums = WidePrefixSums::try_from_values(&[u32::MAX as u64, 7]).unwrap();
        assert_eq!(sums.prefix_before(1).unwrap(), u32::MAX as u64);
        assert_eq!(sums.total().unwrap(), u64::from(u32::MAX) + 7);
        assert_eq!(sums.range(1, 2).unwrap(), 7);
    }

    #[test]
    fn partition_point_is_half_open_and_includes_zero_height_entries() {
        let sums = WidePrefixSums::try_from_values(&[0, 3, 0, 4]).unwrap();
        assert_eq!(sums.partition_point(0), 1);
        assert_eq!(sums.partition_point(3), 3);
        assert_eq!(sums.partition_point(6), 3);
        assert_eq!(sums.partition_point(7), 4);
    }

    #[test]
    fn failed_update_is_atomic() {
        let mut sums = WidePrefixSums::try_from_values(&[u64::MAX]).unwrap();
        assert_eq!(
            sums.add(0, 1),
            Err(WidePrefixError::SumOverflow { index: 0 })
        );
        assert_eq!(sums.value(0).unwrap(), u64::MAX);
    }

    #[test]
    fn construction_rejects_total_overflow() {
        assert_eq!(
            WidePrefixSums::try_from_values(&[u64::MAX, 1]),
            Err(WidePrefixError::SumOverflow { index: 1 })
        );
    }

    #[test]
    fn negative_adjustment_checks_the_entry_not_just_its_nodes() {
        // Node 2 holds 5 + 5 and could absorb -7; entry 1 cannot.
        let mut sums = WidePrefixSums::try_from_values(&[5, 5]).unwrap();
        assert_eq!(
            sums.add(1, -7),
            Err(WidePrefixError::SumUnderflow { index: 1 })
        );
        assert_eq!(sums.value(1).unwrap(), 5);
        assert_eq!(sums.total().unwrap(), 10);
    }

    #[test]
    fn total_overflow_is_rejected_when_no_node_overflows() {
        // No node of `[MAX, 0, 1]` exceeds `u64::MAX`; the prefix through
        // entry 2 does.
        assert_eq!(
            WidePrefixSums::try_from_values(&[u64::MAX, 0, 1]),
            Err(WidePrefixError::SumOverflow { index: 2 })
        );
        let mut sums = WidePrefixSums::try_from_values(&[u64::MAX, 0, 0]).unwrap();
        assert_eq!(
            sums.add(2, 1),
            Err(WidePrefixError::SumOverflow { index: 2 })
        );
        assert_eq!(sums.value(2).unwrap(), 0);
        assert_eq!(sums.total().unwrap(), u64::MAX);
    }

    #[test]
    fn updates_match_a_plain_vector_model() {
        // Near-maximal values make both the entry and the total bounds bite.
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // A new value for `model[index]` is admissible when the total with it
        // still fits in a `u64`.
        let fits = |model: &[u64], index: usize, value: u64| {
            model
                .iter()
                .enumerate()
                .filter(|&(i, _)| i != index)
                .try_fold(value, |sum, (_, &v)| sum.checked_add(v))
                .is_some()
        };
        let mut model = vec![0_u64; 9];
        let mut sums = WidePrefixSums::try_from_values(&model).unwrap();
        for _ in 0..20_000 {
            let index = (next() % 9) as usize;
            let magnitude = next() >> (next() % 64);
            let target = if next() % 2 == 0 {
                let delta = (magnitude >> 1) as i64 * if next() % 2 == 0 { 1 } else { -1 };
                let entry = if delta >= 0 {
                    model[index].checked_add(delta.unsigned_abs())
                } else {
                    model[index].checked_sub(delta.unsigned_abs())
                }
                .filter(|&entry| fits(&model, index, entry));
                assert_eq!(sums.add(index, delta).is_ok(), entry.is_some());
                entry
            } else {
                let entry = Some(magnitude).filter(|&entry| fits(&model, index, entry));
                assert_eq!(sums.set(index, magnitude).is_ok(), entry.is_some());
                entry
            };
            if let Some(entry) = target {
                model[index] = entry;
            }
            for (i, &value) in model.iter().enumerate() {
                assert_eq!(sums.value(i), Ok(value));
            }
        }
    }

    #[test]
    fn negative_adjustment_rejects_underflow() {
        let mut sums = WidePrefixSums::try_from_values(&[2, 4]).unwrap();
        assert_eq!(
            sums.add(0, -3),
            Err(WidePrefixError::SumUnderflow { index: 0 })
        );
        assert_eq!(sums.total().unwrap(), 6);
    }

    #[test]
    fn constructor_and_queries_validate_boundaries() {
        assert_eq!(
            WidePrefixSums::try_new(usize::MAX),
            Err(WidePrefixError::CapacityOverflow { len: usize::MAX })
        );
        let sums = WidePrefixSums::try_new(2).unwrap();
        assert_eq!(
            sums.value(2),
            Err(WidePrefixError::IndexOutOfBounds { index: 2, len: 2 })
        );
        assert_eq!(
            sums.range(2, 1),
            Err(WidePrefixError::InvalidRange {
                start: 2,
                end: 1,
                len: 2
            })
        );
    }

    #[test]
    fn set_and_rebuild_preserve_checked_semantics() {
        let mut sums = WidePrefixSums::try_from_values(&[1, 2, 3]).unwrap();
        sums.set(1, u64::from(u32::MAX) + 1).unwrap();
        assert_eq!(sums.value(1).unwrap(), u64::from(u32::MAX) + 1);
        sums.rebuild(&[5, 8]).unwrap();
        assert_eq!(sums.total().unwrap(), 13);
        assert_eq!(sums.len(), 2);
    }

    #[test]
    fn structural_edits_are_bounded_and_checked() {
        let mut sums = WidePrefixSums::try_from_values(&[2, 4]).unwrap();
        sums.insert(1, 3).unwrap();
        assert_eq!(sums.total().unwrap(), 9);
        assert_eq!(sums.value(1).unwrap(), 3);
        assert_eq!(sums.remove(0).unwrap(), 2);
        assert_eq!(sums.range(0, 2).unwrap(), 7);
        assert_eq!(
            sums.insert(usize::MAX, 1),
            Err(WidePrefixError::IndexOutOfBounds {
                index: usize::MAX,
                len: 2
            })
        );
    }

    #[test]
    fn structural_overflow_preserves_existing_tree() {
        let mut sums = WidePrefixSums::try_from_values(&[u64::MAX]).unwrap();
        assert_eq!(
            sums.insert(1, 1),
            Err(WidePrefixError::SumOverflow { index: 1 })
        );
        assert_eq!(sums.len(), 1);
        assert_eq!(sums.value(0).unwrap(), u64::MAX);
    }
}
