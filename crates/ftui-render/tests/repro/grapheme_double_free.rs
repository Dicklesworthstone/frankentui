use ftui_render::grapheme_pool::GraphemePool;

#[test]
fn test_grapheme_pool_double_free_corruption() {
    let mut pool = GraphemePool::new();
    
    // 1. Intern "A"
    let id_a = pool.intern("A", 1);
    
    // 2. Release "A" (correct) -> Slot 0 added to free list
    pool.release(id_a);
    
    // 3. Release "A" AGAIN (bug) -> Slot 0 added to free list AGAIN
    // Current implementation uses saturating_sub, so refcount stays 0, 
    // and the "if refcount == 0" block runs again.
    pool.release(id_a);
    
    // 4. Intern "B" -> Gets Slot 0 (popped once)
    let id_b = pool.intern("B", 1);
    
    // 5. Intern "C" -> Gets Slot 0 (popped again!)
    let id_c = pool.intern("C", 1);
    
    // 6. Verify fix
    // B should own Slot 0.
    // C should get a NEW slot (Slot 1) because the double-free was ignored.
    assert_ne!(id_b.slot(), id_c.slot(), "Double-free should be ignored, so C gets a new slot");
    
    assert_eq!(pool.get(id_b), Some("B"));
    assert_eq!(pool.get(id_c), Some("C"));
}
