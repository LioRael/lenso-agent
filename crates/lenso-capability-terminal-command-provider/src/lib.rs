//! Portable command catalog and execution role implemented by feature Plugins.

include!("generated.rs");

/// Maximum number of commands one provider may advertise.
pub const MAX_COMMANDS_PER_PROVIDER: usize = 256;

/// Validates the semantic constraints that are stricter than the wire schema.
pub fn validate_catalog(commands: &[CommandDefinition]) -> Result<(), String> {
    if commands.len() > MAX_COMMANDS_PER_PROVIDER {
        return Err(format!(
            "catalog exceeds the {MAX_COMMANDS_PER_PROVIDER}-command provider limit"
        ));
    }

    let mut ids = std::collections::BTreeSet::new();
    let mut paths = std::collections::BTreeSet::<String>::new();
    for command in commands {
        validate_command(command)?;
        if !ids.insert(command.id.as_str()) {
            return Err(format!("duplicate command id `{}`", command.id));
        }
        let path = command.path.join(" ");
        if let Some(existing) = paths
            .iter()
            .find(|existing| path_prefix_conflict(existing, &path))
        {
            return Err(format!(
                "command path `{path}` conflicts with command group `{existing}`"
            ));
        }
        if !paths.insert(path.clone()) {
            return Err(format!("duplicate command path `{path}`"));
        }
    }
    Ok(())
}

fn validate_command(command: &CommandDefinition) -> Result<(), String> {
    if !valid_identifier(&command.id, 128, b"._-") {
        return Err(format!("invalid command id `{}`", command.id));
    }
    if command.path.is_empty()
        || command.path.len() > 8
        || command
            .path
            .iter()
            .any(|segment| !valid_identifier(segment, 64, b"-"))
    {
        return Err(format!("invalid command path `{}`", command.path.join(" ")));
    }
    if command.summary.is_empty()
        || command.summary.chars().count() > 256
        || command.description.chars().count() > 4_096
    {
        return Err(format!("invalid metadata for command `{}`", command.id));
    }
    if command.output_formats.is_empty() || command.output_formats.len() > 2 {
        return Err(format!(
            "command `{}` has invalid output formats",
            command.id
        ));
    }
    let text_count = command
        .output_formats
        .iter()
        .filter(|format| matches!(format, OutputFormat::Text))
        .count();
    let json_count = command
        .output_formats
        .iter()
        .filter(|format| matches!(format, OutputFormat::Json))
        .count();
    if text_count > 1 || json_count > 1 {
        return Err(format!("command `{}` repeats an output format", command.id));
    }
    validate_parameters(command)
}

fn validate_parameters(command: &CommandDefinition) -> Result<(), String> {
    if command.parameters.len() > 64 {
        return Err(format!("command `{}` has too many parameters", command.id));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut longs = std::collections::BTreeSet::new();
    let mut shorts = std::collections::BTreeSet::new();
    let mut optional_positional_seen = false;
    let mut repeated_positional_seen = false;

    for parameter in &command.parameters {
        if !valid_identifier(&parameter.id, 64, b"_") || !ids.insert(parameter.id.as_str()) {
            return Err(format!(
                "command `{}` has invalid or duplicate parameter `{}`",
                command.id, parameter.id
            ));
        }
        if parameter.description.chars().count() > 1_024
            || parameter.choices.len() > 64
            || parameter
                .choices
                .iter()
                .any(|choice| choice.is_empty() || choice.chars().count() > 128)
        {
            return Err(format!(
                "command `{}` has invalid parameter metadata for `{}`",
                command.id, parameter.id
            ));
        }
        let mut unique_choices = std::collections::BTreeSet::new();
        if parameter
            .choices
            .iter()
            .any(|choice| !unique_choices.insert(choice.as_str()))
        {
            return Err(format!(
                "command `{}` repeats a choice for `{}`",
                command.id, parameter.id
            ));
        }

        let long = optional_string(parameter.long.as_ref().map(Option::as_ref));
        let short = optional_string(parameter.short.as_ref().map(Option::as_ref));
        let value_name = optional_string(parameter.value_name.as_ref().map(Option::as_ref));
        if value_name.is_some_and(|name| name.is_empty() || name.chars().count() > 64) {
            return Err(format!(
                "parameter `{}` on `{}` has an invalid value_name",
                parameter.id, command.id
            ));
        }
        match parameter.kind {
            ParameterKind::Positional => {
                if long.is_some() || short.is_some() || value_name.is_none() {
                    return Err(format!(
                        "positional parameter `{}` on `{}` has option metadata",
                        parameter.id, command.id
                    ));
                }
                if optional_positional_seen && parameter.required {
                    return Err(format!(
                        "required positional `{}` follows an optional positional on `{}`",
                        parameter.id, command.id
                    ));
                }
                if repeated_positional_seen {
                    return Err(format!(
                        "positional `{}` follows a repeated positional on `{}`",
                        parameter.id, command.id
                    ));
                }
                optional_positional_seen |= !parameter.required;
                repeated_positional_seen |= parameter.multiple;
            }
            ParameterKind::Option => {
                validate_option_names(command, parameter, long, short, &mut longs, &mut shorts)?;
                if value_name.is_none() {
                    return Err(format!(
                        "option `{}` on `{}` is missing value_name",
                        parameter.id, command.id
                    ));
                }
            }
            ParameterKind::Flag => {
                validate_option_names(command, parameter, long, short, &mut longs, &mut shorts)?;
                if value_name.is_some()
                    || !parameter.choices.is_empty()
                    || parameter.multiple
                    || parameter.required
                {
                    return Err(format!(
                        "flag `{}` on `{}` has value semantics",
                        parameter.id, command.id
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_option_names<'a>(
    command: &CommandDefinition,
    parameter: &'a CommandParameter,
    long: Option<&'a str>,
    short: Option<&'a str>,
    longs: &mut std::collections::BTreeSet<&'a str>,
    shorts: &mut std::collections::BTreeSet<&'a str>,
) -> Result<(), String> {
    let Some(long) = long else {
        return Err(format!(
            "parameter `{}` on `{}` is missing a long option name",
            parameter.id, command.id
        ));
    };
    if long == "json" || !valid_identifier(long, 64, b"-") || !longs.insert(long) {
        return Err(format!(
            "parameter `{}` on `{}` has an invalid, reserved, or duplicate long option",
            parameter.id, command.id
        ));
    }
    if let Some(short) = short {
        let valid =
            short.len() == 1 && short.as_bytes()[0].is_ascii_alphanumeric() && shorts.insert(short);
        if !valid {
            return Err(format!(
                "parameter `{}` on `{}` has an invalid or duplicate short option",
                parameter.id, command.id
            ));
        }
    }
    Ok(())
}

fn optional_string(value: OptionalValue<&String>) -> Option<&str> {
    value.flatten().map(String::as_str)
}

fn valid_identifier(value: &str, max: usize, punctuation: &[u8]) -> bool {
    let bytes = value.as_bytes();
    matches!(bytes.first(), Some(b'a'..=b'z'))
        && bytes.len() <= max
        && bytes[1..].iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || punctuation.iter().any(|allowed| allowed == byte)
        })
}

fn path_prefix_conflict(left: &str, right: &str) -> bool {
    left.strip_prefix(right)
        .is_some_and(|suffix| suffix.starts_with(' '))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with(' '))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command() -> CommandDefinition {
        CommandDefinition {
            id: "session.list".to_owned(),
            path: vec!["sessions".to_owned(), "list".to_owned()],
            summary: "List sessions".to_owned(),
            description: String::new(),
            parameters: Vec::new(),
            output_formats: vec![OutputFormat::Text, OutputFormat::Json],
        }
    }

    #[test]
    fn accepts_a_nested_command() {
        assert!(validate_catalog(&[command()]).is_ok());
    }

    #[test]
    fn rejects_duplicate_paths_even_when_ids_differ() {
        let first = command();
        let mut second = command();
        second.id = "session.list-alias".to_owned();
        assert!(validate_catalog(&[first, second]).is_err());
    }

    #[test]
    fn reserves_json_for_surface_output_selection() {
        let mut value = command();
        value.parameters.push(CommandParameter {
            id: "json".to_owned(),
            kind: ParameterKind::Flag,
            long: Some(Some("json".to_owned())),
            short: None,
            value_name: None,
            description: String::new(),
            required: false,
            multiple: false,
            choices: Vec::new(),
        });
        assert!(validate_catalog(&[value]).is_err());
    }
}
