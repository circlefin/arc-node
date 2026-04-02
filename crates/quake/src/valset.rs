// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::str::FromStr;

use crate::node::NodeName;

/// A validator power update consisting of a validator name and new voting power.
/// This is what quake's `valset` command parses from a string of the form
/// [<validator>:<voting_power>] given as input to the command.
#[derive(Debug, Clone)]
pub(crate) struct ValidatorPowerUpdate {
    /// Validator identifier, e.g., validator1
    pub(crate) validator_name: NodeName,
    /// New voting power for the validator, e.g., 42
    pub(crate) new_voting_power: u64,
}

impl FromStr for ValidatorPowerUpdate {
    type Err = String;

    /// Parses a string of the form `<validator>:<voting_power>` into a
    /// `ValidatorUpdate`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (validator, voting_power) = s
            .split_once(':')
            .ok_or_else(|| format!("expected <validator>:<voting_power>, got '{s}'"))?;

        let number = voting_power
            .parse::<u64>()
            .map_err(|e| format!("invalid number in '{s}': {e}"))?;

        let update = ValidatorPowerUpdate {
            validator_name: validator.to_string(),
            new_voting_power: number,
        };

        Ok(update)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_validator_update() {
        let update: ValidatorPowerUpdate = "validator1:42".parse().unwrap();
        assert_eq!(update.validator_name, "validator1");
        assert_eq!(update.new_voting_power, 42);
    }

    #[test]
    fn error_on_missing_separator() {
        let result: Result<ValidatorPowerUpdate, _> = "validator-blue42".parse();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("expected <validator>:<voting_power>"));
    }

    #[test]
    fn error_on_empty_string() {
        let result: Result<ValidatorPowerUpdate, _> = "".parse();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("expected <validator>:<voting_power>"));
    }

    #[test]
    fn error_on_invalid_voting_power() {
        let result: Result<ValidatorPowerUpdate, _> = "validator-green:not_a_number".parse();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid number"));
    }

    #[test]
    fn error_on_negative_voting_power() {
        let result: Result<ValidatorPowerUpdate, _> = "validator2:-5".parse();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid number"));
    }

    #[test]
    fn error_on_empty_voting_power() {
        let result: Result<ValidatorPowerUpdate, _> = "validator-yellow:".parse();
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid number"));
    }

    #[test]
    fn parse_empty_validator_name() {
        let update: ValidatorPowerUpdate = ":100".parse().unwrap();
        assert_eq!(update.validator_name, "");
        assert_eq!(update.new_voting_power, 100);
    }
}
