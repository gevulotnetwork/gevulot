use std::{
    path::{Path, PathBuf},
    process::Command,
};

use eyre::Result;
use thiserror::Error;

#[allow(clippy::enum_variant_names)]
#[derive(Error, Debug)]
pub enum NanosVolumeError {
    #[error("ops error: {0}")]
    OpsError(String),

    #[error("parse error: {0}")]
    ParseError(String),
}

pub fn create(data_dir: &Path, label: &str, size: &str) -> Result<PathBuf> {
    let output = Command::new("ops")
        .env("HOME", data_dir)
        .arg("volume")
        .arg("create")
        .arg(label)
        .arg("-s")
        .arg(size)
        .output()?;

    if !output.status.success() {
        return Err(
            NanosVolumeError::OpsError(String::from("failed to run 'ops volume create'")).into(),
        );
    }

    let volume_file = parse_output(&String::from_utf8(output.stderr)?)?;
    Ok(PathBuf::new()
        .join(home::home_dir().expect("missing $HOME"))
        .join(".ops")
        .join("volumes")
        .join(volume_file))
}

pub fn delete(label: &str) -> Result<()> {
    let output = Command::new("ops")
        .arg("volume")
        .arg("delete")
        .arg(label)
        .output()?;

    if !output.status.success() {
        return Err(
            NanosVolumeError::OpsError(String::from("failed to run 'ops volume delete'")).into(),
        );
    }

    Ok(())
}

fn parse_output(output: &str) -> Result<String> {
    let tokens: Vec<String> = output
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();

    if tokens.is_empty() {
        return Err(NanosVolumeError::ParseError(String::from("empty string")).into());
    }

    let mut uuid: Option<String> = None;
    let mut label: Option<String> = None;

    let mut i = 0;
    while i < (tokens.len() - 1) {
        match tokens[i].as_str() {
            "uuid" => {
                i += 1;
                uuid = Some(tokens[i].clone());
            }
            "label" => {
                i += 1;
                label = Some(tokens[i].clone());
            }
            _ => (),
        }

        i += 1
    }

    if uuid.is_some() && label.is_some() {
        return Ok(format!("{}:{}.raw", label.unwrap(), uuid.unwrap()));
    }

    Err(NanosVolumeError::ParseError(
        "couldn't find all required components from 'ops volume create' output".to_string(),
    )
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_output() {
        let output = "2023/12/22 14:58:10 volume: abcdefghijklmn created with UUID 3d30257d-650b-4b22-b68d-f85b312a0956 and label abcdefghijklmn";
        let res = parse_output(output);
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            "abcdefghijklmn:3d30257d-650b-4b22-b68d-f85b312a0956.raw"
        );
    }

    #[test]
    fn test_parse_output_no_uuid() {
        let output = "2023/12/22 14:58:10 volume: abcdefghijklmn created with 3d30257d-650b-4b22-b68d-f85b312a0956 and label abcdefghijklmn";
        let res = parse_output(output);
        assert!(res.is_err());
    }

    #[test]
    fn test_parse_output_no_label() {
        let output = "2023/12/22 14:58:10 volume: abcdefghijklmn created with UUID 3d30257d-650b-4b22-b68d-f85b312a0956 and abcdefghijklmn";
        let res = parse_output(output);
        assert!(res.is_err());
    }

    #[test]
    fn test_parse_output_unexpected_end_of_str() {
        let output = "2023/12/22 14:58:10 volume: abcdefghijklmn created with UUID 3d30257d-650b-4b22-b68d-f85b312a0956 and label";
        let res = parse_output(output);
        assert!(res.is_err());
    }

    #[test]
    fn test_parse_output_unexpected_order() {
        let output = "2023/12/22 14:58:10 volume: abcdefghijklmn created and label abcdefghijklmn with UUID 3d30257d-650b-4b22-b68d-f85b312a0956";
        let res = parse_output(output);
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            "abcdefghijklmn:3d30257d-650b-4b22-b68d-f85b312a0956.raw"
        );
    }

    #[test]
    fn test_parse_output_unexpected_content() {
        let output = "     created and label abcdefghijklmn with very unusual and obscure <>|! UUID 3d30257d-650b-4b22-b68d-f85b312a0956        ";
        let res = parse_output(output);
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            "abcdefghijklmn:3d30257d-650b-4b22-b68d-f85b312a0956.raw"
        );
    }
}
