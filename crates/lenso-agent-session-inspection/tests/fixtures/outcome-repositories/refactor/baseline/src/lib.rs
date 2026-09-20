pub fn display_name(value: &str) -> String {
    value.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn preserves_the_public_normalization_behavior() {
        assert_eq!(display_name("  Lenso Agent  "), "lenso agent");
    }
}
