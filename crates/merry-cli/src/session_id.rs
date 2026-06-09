pub(crate) fn new_ephemeral_session_id() -> merry_core::SessionId {
    merry_core::SessionId::random()
}

#[cfg(test)]
mod tests {
    use super::new_ephemeral_session_id;

    #[test]
    fn ephemeral_session_ids_are_random_and_not_product_labels() {
        let first = new_ephemeral_session_id();
        let second = new_ephemeral_session_id();

        assert_ne!(first, second);
        assert_ne!(first.as_str(), "run");
        assert_ne!(first.as_str(), "cmd");
        assert_eq!(first.as_str().len(), 36);
        assert_eq!(second.as_str().len(), 36);
    }
}
