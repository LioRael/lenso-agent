pub fn parse_port(value: &str) -> Result<u16, &'static str> {
    let port = value.parse::<u16>().map_err(|_| "invalid port")?;
    (port != 0).then_some(port).ok_or("port must be nonzero")
}

#[cfg(test)]
mod tests {
    use super::parse_port;

    #[test]
    fn accepts_a_port_with_operator_whitespace() {
        assert_eq!(parse_port(" 8080 "), Ok(8080));
    }

    #[test]
    fn rejects_the_reserved_zero_port() {
        assert_eq!(parse_port("0"), Err("port must be nonzero"));
    }
}
