use std::collections::BTreeMap;

use crate::error::{DoctorError, Result};

#[derive(Debug, Clone, Default)]
pub struct Profile {
    pub name: String,
    pub values: BTreeMap<String, String>,
}

impl Profile {
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    #[must_use]
    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.get(key).map(|raw| {
            let value = raw.trim().to_ascii_lowercase();
            matches!(value.as_str(), "1" | "true" | "yes" | "on")
        })
    }

    #[must_use]
    pub fn get_u16(&self, key: &str) -> Option<u16> {
        self.get(key).and_then(|raw| raw.trim().parse::<u16>().ok())
    }

    #[must_use]
    pub fn get_u32(&self, key: &str) -> Option<u32> {
        self.get(key).and_then(|raw| raw.trim().parse::<u32>().ok())
    }

    #[must_use]
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(|raw| raw.trim().parse::<u64>().ok())
    }
}

const ANALYTICS_EMPTY: &str = include_str!("../profiles/analytics-empty.env");
const ANALYTICS_SEEDED: &str = include_str!("../profiles/analytics-seeded.env");
const MESSAGES_SEEDED: &str = include_str!("../profiles/messages-seeded.env");
const TOUR_SEEDED: &str = include_str!("../profiles/tour-seeded.env");

const BUILTIN_PROFILES: [(&str, &str); 4] = [
    ("analytics-empty", ANALYTICS_EMPTY),
    ("analytics-seeded", ANALYTICS_SEEDED),
    ("messages-seeded", MESSAGES_SEEDED),
    ("tour-seeded", TOUR_SEEDED),
];

#[must_use]
pub fn list_profile_names() -> Vec<String> {
    BUILTIN_PROFILES
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect()
}

pub fn load_profile(name: &str) -> Result<Profile> {
    let (_, content) = BUILTIN_PROFILES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .ok_or_else(|| DoctorError::ProfileNotFound {
            name: name.to_string(),
        })?;

    let values = parse_profile_content(content);

    Ok(Profile {
        name: name.to_string(),
        values,
    })
}

#[must_use]
pub fn parse_profile_content(content: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some((key, value_raw)) = line.split_once('=') else {
            continue;
        };

        let key = key.trim().to_string();
        let mut value = value_raw.trim().to_string();

        if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
            value = value[1..value.len() - 1].to_string();
        }

        values.insert(key, value);
    }

    values
}

#[cfg(test)]
mod tests {
    use super::{list_profile_names, parse_profile_content};

    #[test]
    fn parse_env_fragment() {
        let parsed = parse_profile_content(
            r#"
                # comment
                key1=value1
                key2="value 2"
            "#,
        );

        assert_eq!(parsed.get("key1"), Some(&"value1".to_string()));
        assert_eq!(parsed.get("key2"), Some(&"value 2".to_string()));
    }

    #[test]
    fn builtins_listed() {
        let names = list_profile_names();
        assert_eq!(names.len(), 4);
        assert!(names.contains(&"analytics-empty".to_string()));
        assert!(names.contains(&"tour-seeded".to_string()));
    }
}
