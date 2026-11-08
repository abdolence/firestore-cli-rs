use crate::describe::SampleSize;
use crate::errors::{FsqError, FsqResult, InvalidParameter, ParameterProblem};
use firestore::{FirestoreDb, FirestoreSqlOptions, FirestoreSqlParameters, FirestoreSqlStatement};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, clap::Args)]
pub struct CodegenArgs {
    #[arg(
        value_name = "SELECT",
        help = "The SELECT to show the fluent Rust code of"
    )]
    pub sql: String,

    #[command(flatten)]
    pub options: CodegenOptions,
}

#[derive(Debug, Clone, PartialEq, clap::Args)]
pub struct CodegenOptions {
    #[arg(
        long = "param",
        value_name = "NAME=VALUE",
        help = "Bind @name to a JSON literal, which gives the argument its type: 4, 4.0, true, \"text\""
    )]
    pub parameters: Vec<ParameterBinding>,

    #[arg(
        long,
        value_name = "N",
        conflicts_with = "offline",
        default_value_t = SampleSize::CODEGEN_DEFAULT,
        help = "Run the SELECT for at most this many rows to type the row struct"
    )]
    pub sample: SampleSize,

    #[arg(
        long,
        help = "Do not run the SELECT: every field is typed as unknown JSON"
    )]
    pub offline: bool,
}

impl CodegenOptions {
    /// How many rows to run the SELECT for, or `None` when `--offline` skips the run.
    pub fn sample_rows(&self) -> Option<SampleSize> {
        (!self.offline).then_some(self.sample)
    }

    /// The code of `sql` with every field of the row struct typed as unknown, from the text alone.
    pub fn offline(&self, sql: &str) -> FsqResult<String> {
        Ok(FirestoreSqlStatement::fluent_rust(
            sql,
            &FirestoreSqlParameters::from(self),
            &[],
        )?)
    }

    /// The code of `sql`, with the row struct typed from the first `rows` rows that `db` returns
    /// for it. The statement is checked before it runs, so that a statement that is not a
    /// SELECT is refused and never sent.
    pub async fn sampled(
        &self,
        db: &FirestoreDb,
        sql: &str,
        rows: SampleSize,
    ) -> FsqResult<String> {
        let parameters = FirestoreSqlParameters::from(self);
        FirestoreSqlStatement::fluent_rust(sql, &parameters, &[])?;
        let sample = self
            .parameters
            .iter()
            .fold(db.fluent().sql(sql), |builder, parameter| {
                builder.param(parameter.name.clone(), &parameter.value)
            })
            .options(FirestoreSqlOptions::read_only().with_max_rows(rows.documents()))
            .query()
            .await?;
        Ok(FirestoreSqlStatement::fluent_rust(
            sql,
            &parameters,
            &sample,
        )?)
    }
}

impl From<&CodegenOptions> for FirestoreSqlParameters {
    fn from(options: &CodegenOptions) -> Self {
        options
            .parameters
            .iter()
            .fold(Self::new(), |parameters, parameter| {
                parameters.param(parameter.name.clone(), &parameter.value)
            })
    }
}

/// A `--param`: the name of a `@name` parameter and the JSON literal bound to it.
#[derive(Debug, Clone, PartialEq)]
pub struct ParameterBinding {
    pub name: String,
    pub value: serde_json::Value,
}

impl FromStr for ParameterBinding {
    type Err = FsqError;

    fn from_str(argument: &str) -> FsqResult<Self> {
        let invalid = |problem| {
            FsqError::InvalidParameter(InvalidParameter {
                argument: argument.to_string(),
                problem,
            })
        };
        let (name, value) = argument
            .split_once('=')
            .ok_or_else(|| invalid(ParameterProblem::MissingValue))?;
        if name.trim().is_empty() {
            return Err(invalid(ParameterProblem::EmptyName));
        }
        Ok(Self {
            name: name.trim().to_string(),
            value: serde_json::from_str(value.trim())
                .map_err(|source| invalid(ParameterProblem::NotJson(source)))?,
        })
    }
}

/// The arguments of `\gen`: a SELECT, or none for the last one that ran, and the options of
/// `fsq codegen`.
#[derive(Debug, Clone, PartialEq, clap::Args)]
pub struct GenerateCommand {
    #[arg(
        value_name = "SELECT",
        help = "The SELECT to show the fluent Rust code of; the last one that ran when it is left out"
    )]
    pub sql: Option<String>,

    #[command(flatten)]
    pub options: CodegenOptions,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn binding(argument: &str) -> ParameterBinding {
        argument.parse().expect(argument)
    }

    #[test]
    fn a_parameter_value_is_a_json_literal_that_keeps_its_type() {
        assert_eq!(binding("n=4").value, json!(4));
        assert_eq!(binding("min=4.0").value, json!(4.0));
        assert_eq!(binding("genre=\"sci fi\"").value, json!("sci fi"));
        assert_eq!(binding("subtitle=null").value, json!(null));
        assert_eq!(binding("min = 4.0").name, "min");
    }

    #[test]
    fn a_parameter_needs_a_name_and_a_json_value() {
        let problem = |argument: &str| match argument.parse::<ParameterBinding>() {
            Err(FsqError::InvalidParameter(InvalidParameter { problem, .. })) => problem,
            other => panic!("{other:?}"),
        };

        assert!(matches!(problem("min"), ParameterProblem::MissingValue));
        assert!(matches!(problem("=4"), ParameterProblem::EmptyName));
        assert!(matches!(
            problem("genre=Dystopian"),
            ParameterProblem::NotJson(_)
        ));
    }
}
