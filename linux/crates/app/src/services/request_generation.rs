pub fn is_current(scheduled: u64, current: u64) -> bool {
    scheduled == current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancelled_generation_must_not_apply() {
        assert!(!is_current(1, 2));
        assert!(!is_current(0, 1));
    }

    #[test]
    fn a_matching_generation_applies() {
        assert!(is_current(0, 0));
        assert!(is_current(3, 3));
    }
}
