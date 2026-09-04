use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Take};
use std::path::Path;
use std::str::FromStr;

use num_bigint::BigInt;
use num_traits::Zero;
use regex::Regex;
use serde_json::{json, Map, Number, Value};

use super::AppError;

pub const MAX_MANIFEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct ManifestTool {
    pub name: String,
    pub summary: String,
    pub args: Vec<ManifestArgument>,
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct ManifestArgument {
    name: String,
    kind: ArgumentKind,
    required: bool,
    repeatable: bool,
    choices: Vec<Value>,
    default: Option<Value>,
    required_when: Option<Condition>,
    label: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgumentKind {
    String,
    Number,
    Integer,
    Bool,
}

#[derive(Debug, Clone)]
struct Condition {
    kind: ConditionKind,
    arg: String,
    value: Option<Value>,
}

#[derive(Debug, Clone, Copy)]
enum ConditionKind {
    Present,
    Equals,
    NotEquals,
}

pub(crate) struct Manifest {
    pub id: String,
    pub version: String,
    pub tools: Vec<ManifestTool>,
}

pub(crate) fn load(path: &Path) -> Result<Manifest, AppError> {
    let file = File::open(path).map_err(|error| {
        AppError::Manifest(format!(
            "cannot read App manifest `{}`: {error}",
            path.display()
        ))
    })?;
    let mut reader: Take<File> = file.take((MAX_MANIFEST_BYTES + 1) as u64);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|error| {
        AppError::Manifest(format!(
            "cannot read App manifest `{}`: {error}",
            path.display()
        ))
    })?;
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(AppError::Manifest(format!(
            "App manifest `{}` exceeds {MAX_MANIFEST_BYTES} bytes",
            path.display()
        )));
    }
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::Manifest(format!(
            "invalid App manifest `{}`: {error}",
            path.display()
        ))
    })?;
    parse(manifest)
}

fn reject_unknown_fields(
    object: &Map<String, Value>,
    context: &str,
    allowed: &[&str],
) -> Result<(), AppError> {
    if let Some(field) = object
        .keys()
        .filter(|field| !allowed.contains(&field.as_str()))
        .min()
    {
        return Err(AppError::Manifest(format!(
            "{context} contains unknown field `{field}`"
        )));
    }
    Ok(())
}

fn parse(manifest: Value) -> Result<Manifest, AppError> {
    let object = manifest
        .as_object()
        .ok_or_else(|| AppError::Manifest("App manifest must be a JSON object".into()))?;
    reject_unknown_fields(
        object,
        "App manifest",
        &[
            "id",
            "version",
            "schema_version",
            "name",
            "summary",
            "icon",
            "runtime",
            "entry",
            "operations",
            "ai",
            "mcp",
            "desktop",
            "dependencies",
        ],
    )?;
    if !object
        .get("schema_version")
        .is_some_and(|value| integer_equals(value, 2))
    {
        return Err(AppError::Manifest(
            "MCP Apps require `schema_version: 2`".into(),
        ));
    }
    let id = required_string(object, "id", "App manifest has no valid `id`")?;
    let id_pattern = Regex::new(r"^[a-z][a-z0-9_-]*$").expect("static App id regex must compile");
    if !id_pattern.is_match(id) {
        return Err(AppError::Manifest("App manifest has no valid `id`".into()));
    }
    let version = required_string(object, "version", "App manifest has no valid `version`")?;
    if version.trim().is_empty() {
        return Err(AppError::Manifest(
            "App manifest has no valid `version`".into(),
        ));
    }
    localized_english(object.get("name"), "name")?;
    let service = object
        .get("mcp")
        .and_then(Value::as_object)
        .ok_or_else(|| AppError::Manifest("App manifest has no `mcp` service".into()))?;
    reject_unknown_fields(
        service,
        "`mcp`",
        &["entry", "transport", "lifecycle", "access", "tools"],
    )?;
    if service
        .get("transport")
        .is_some_and(|transport| transport.as_str() != Some("stdio"))
    {
        return Err(AppError::Manifest("`mcp.transport` must be `stdio`".into()));
    }
    if service.get("lifecycle").is_some_and(|lifecycle| {
        !matches!(
            lifecycle.as_str(),
            Some("lazy" | "always-on" | "while-app-running")
        )
    }) {
        return Err(AppError::Manifest("`mcp.lifecycle` is invalid".into()));
    }
    if let Some(access) = service.get("access") {
        let access = access
            .as_object()
            .ok_or_else(|| AppError::Manifest("`mcp.access` must be an object".into()))?;
        reject_unknown_fields(
            access,
            "`mcp.access`",
            &["system_agent", "apps", "external_agents"],
        )?;
    }
    let raw_tools = match service.get("tools") {
        Some(Value::Array(tools)) => tools.as_slice(),
        Some(_) => return Err(AppError::Manifest("`mcp.tools` must be an array".into())),
        None => &[],
    };
    if raw_tools.is_empty() {
        return Err(AppError::Manifest(
            "`mcp.tools` must contain at least one tool".into(),
        ));
    }
    let tool_pattern =
        Regex::new(r"^[a-z][a-z0-9._-]*$").expect("static tool name regex must compile");
    let mut names = HashSet::new();
    let mut tools = Vec::with_capacity(raw_tools.len());
    for (index, raw_tool) in raw_tools.iter().enumerate() {
        let tool = raw_tool
            .as_object()
            .ok_or_else(|| AppError::Manifest(format!("`mcp.tools[{index}]` must be an object")))?;
        reject_unknown_fields(
            tool,
            &format!("`mcp.tools[{index}]`"),
            &["name", "summary", "args", "needs"],
        )?;
        let name = tool
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::Manifest(format!("`mcp.tools[{index}].name` is invalid")))?;
        if !tool_pattern.is_match(name) {
            return Err(AppError::Manifest(format!(
                "`mcp.tools[{index}].name` is invalid"
            )));
        }
        if !names.insert(name.to_string()) {
            return Err(AppError::Manifest(format!(
                "tool `{name}` is declared twice"
            )));
        }
        let summary =
            localized_english(tool.get("summary"), &format!("mcp.tools[{index}].summary"))?;
        let raw_args = match tool.get("args") {
            Some(Value::Array(args)) => args.as_slice(),
            Some(_) => {
                return Err(AppError::Manifest(format!(
                    "tool `{name}` args must be an array"
                )));
            }
            None => &[],
        };
        let args = parse_arguments(name, raw_args)?;
        let input_schema = build_input_schema(&args);
        tools.push(ManifestTool {
            name: name.to_string(),
            summary,
            args,
            input_schema,
        });
    }
    Ok(Manifest {
        id: id.to_string(),
        version: version.to_string(),
        tools,
    })
}

fn parse_arguments(tool_name: &str, raw_args: &[Value]) -> Result<Vec<ManifestArgument>, AppError> {
    let mut args = Vec::with_capacity(raw_args.len());
    let mut earlier = HashMap::new();
    for (index, raw) in raw_args.iter().enumerate() {
        let object = raw.as_object().ok_or_else(|| {
            AppError::Manifest(format!("tool `{tool_name}` arg {index} must be an object"))
        })?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !name.trim().is_empty())
            .ok_or_else(|| {
                AppError::Manifest(format!("tool `{tool_name}` arg {index} has no valid name"))
            })?;
        if earlier.contains_key(name) {
            return Err(AppError::Manifest(format!(
                "tool `{tool_name}` arg `{name}` is duplicated"
            )));
        }
        let kind = match object.get("kind").and_then(Value::as_str) {
            Some("path" | "host" | "name" | "text") => ArgumentKind::String,
            Some("number") => ArgumentKind::Number,
            Some("integer") => ArgumentKind::Integer,
            Some("bool") => ArgumentKind::Bool,
            _ => {
                return Err(AppError::Manifest(format!(
                    "tool `{tool_name}` arg `{name}` has invalid kind"
                )));
            }
        };
        reject_unknown_fields(
            object,
            &format!("tool `{tool_name}` arg `{name}`"),
            &[
                "name",
                "kind",
                "required",
                "required_when",
                "repeatable",
                "choices",
                "default",
                "label",
            ],
        )?;
        let required = optional_bool(object, "required", tool_name, name)?;
        let repeatable = optional_bool(object, "repeatable", tool_name, name)?;
        if repeatable && kind == ArgumentKind::Bool {
            return Err(AppError::Manifest(format!(
                "tool `{tool_name}` arg `{name}` cannot repeat booleans"
            )));
        }

        let choices = match object.get("choices") {
            Some(Value::Array(choices)) => choices.clone(),
            Some(_) => {
                return Err(AppError::Manifest(format!(
                    "tool `{tool_name}` arg `{name}` choices must be an array"
                )));
            }
            None => Vec::new(),
        };
        for (choice_index, choice) in choices.iter().enumerate() {
            validate_scalar(name, kind, choice).map_err(|message| {
                AppError::Manifest(format!("choice {choice_index} for `{name}`: {message}"))
            })?;
            if choices[..choice_index]
                .iter()
                .any(|prior| values_equal(prior, choice))
            {
                return Err(AppError::Manifest(format!(
                    "tool `{tool_name}` arg `{name}` choices must be unique"
                )));
            }
        }
        let default = object.get("default").cloned();
        if required && default.is_some() {
            return Err(AppError::Manifest(format!(
                "tool `{tool_name}` arg `{name}` cannot be required and defaulted"
            )));
        }
        let required_when = object
            .get("required_when")
            .map(|condition| parse_condition(tool_name, name, condition, &earlier))
            .transpose()?;
        if required_when.is_some() && (required || repeatable || default.is_some()) {
            return Err(AppError::Manifest(format!(
                "tool `{tool_name}` arg `{name}` has an incompatible required_when declaration"
            )));
        }
        if let Some(default_value) = default.as_ref() {
            if repeatable {
                let values = default_value.as_array().ok_or_else(|| {
                    AppError::Manifest(format!("default for `{name}` must be an array"))
                })?;
                for value in values {
                    validate_scalar(name, kind, value)
                        .map_err(|message| AppError::Manifest(format!("default {message}")))?;
                    if !choices.is_empty()
                        && !choices.iter().any(|choice| values_equal(choice, value))
                    {
                        return Err(AppError::Manifest(format!(
                            "tool `{tool_name}` arg `{name}` default is not an allowed choice"
                        )));
                    }
                }
            } else {
                validate_scalar(name, kind, default_value)
                    .map_err(|message| AppError::Manifest(format!("default {message}")))?;
                if !choices.is_empty()
                    && !choices
                        .iter()
                        .any(|choice| values_equal(choice, default_value))
                {
                    return Err(AppError::Manifest(format!(
                        "tool `{tool_name}` arg `{name}` default is not an allowed choice"
                    )));
                }
            }
        }
        let label = object
            .get("label")
            .map(|value| {
                localized_english(
                    Some(value),
                    &format!("tool `{tool_name}` arg `{name}` label"),
                )
            })
            .transpose()?;
        args.push(ManifestArgument {
            name: name.to_string(),
            kind,
            required,
            repeatable,
            choices,
            default,
            required_when,
            label,
        });
        earlier.insert(name.to_string(), kind);
    }
    Ok(args)
}

fn parse_condition(
    tool_name: &str,
    arg_name: &str,
    raw: &Value,
    earlier: &HashMap<String, ArgumentKind>,
) -> Result<Condition, AppError> {
    let object = raw.as_object().ok_or_else(|| {
        AppError::Manifest(format!(
            "tool `{tool_name}` arg `{arg_name}` required_when must be an object"
        ))
    })?;
    if let Some(field) = object
        .keys()
        .find(|field| !matches!(field.as_str(), "kind" | "arg" | "value"))
    {
        return Err(AppError::Manifest(format!(
            "tool `{tool_name}` arg `{arg_name}` required_when has unknown field `{field}`"
        )));
    }
    let kind = match object.get("kind").and_then(Value::as_str) {
        Some("arg-present") => ConditionKind::Present,
        Some("arg-equals") => ConditionKind::Equals,
        Some("arg-not-equals") => ConditionKind::NotEquals,
        _ => {
            return Err(AppError::Manifest(format!(
                "tool `{tool_name}` arg `{arg_name}` has invalid required_when kind"
            )));
        }
    };
    let source = object
        .get("arg")
        .and_then(Value::as_str)
        .filter(|source| earlier.contains_key(*source))
        .ok_or_else(|| {
            AppError::Manifest(format!(
                "tool `{tool_name}` arg `{arg_name}` required_when must reference an earlier arg"
            ))
        })?;
    let value = object.get("value").cloned();
    match kind {
        ConditionKind::Present if value.is_some() => {
            return Err(AppError::Manifest(format!(
                "tool `{tool_name}` arg `{arg_name}` arg-present cannot declare value"
            )));
        }
        ConditionKind::Equals | ConditionKind::NotEquals
            if value.as_ref().is_none_or(Value::is_null) =>
        {
            return Err(AppError::Manifest(format!(
                "tool `{tool_name}` arg `{arg_name}` condition requires a non-null value"
            )));
        }
        _ => {}
    }
    Ok(Condition {
        kind,
        arg: source.to_string(),
        value,
    })
}

fn build_input_schema(args: &[ManifestArgument]) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut all_of = Vec::new();
    for arg in args {
        let mut scalar = Map::new();
        scalar.insert("type".into(), Value::String(json_type(arg.kind).into()));
        if !arg.choices.is_empty() {
            scalar.insert("enum".into(), Value::Array(arg.choices.clone()));
        }
        let mut property = if arg.repeatable {
            Map::from_iter([
                ("type".into(), Value::String("array".into())),
                ("items".into(), Value::Object(scalar)),
            ])
        } else {
            scalar
        };
        if let Some(label) = arg.label.as_ref() {
            property.insert("description".into(), Value::String(label.clone()));
        }
        if let Some(default) = arg.default.as_ref() {
            property.insert("default".into(), default.clone());
        }
        properties.insert(arg.name.clone(), Value::Object(property));
        if arg.required {
            required.push(Value::String(arg.name.clone()));
        }
        if let Some(condition) = arg.required_when.as_ref() {
            all_of.push(json!({
                "if": condition_schema(condition),
                "then": {"required": [arg.name.clone()]},
                "else": {"not": {"required": [arg.name.clone()]}}
            }));
        }
    }
    let mut schema = Map::from_iter([
        ("type".into(), Value::String("object".into())),
        ("properties".into(), Value::Object(properties)),
        ("additionalProperties".into(), Value::Bool(false)),
    ]);
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }
    if !all_of.is_empty() {
        schema.insert("allOf".into(), Value::Array(all_of));
    }
    Value::Object(schema)
}

fn condition_schema(condition: &Condition) -> Value {
    match condition.kind {
        ConditionKind::Present => json!({"required": [condition.arg.clone()]}),
        ConditionKind::Equals => json!({
            "properties": {
                condition.arg.clone(): {"const": condition.value.clone()}
            },
            "required": [condition.arg.clone()]
        }),
        ConditionKind::NotEquals => json!({
            "required": [condition.arg.clone()],
            "not": {
                "properties": {
                    condition.arg.clone(): {"const": condition.value.clone()}
                },
                "required": [condition.arg.clone()]
            }
        }),
    }
}

pub(crate) fn resolve_arguments(
    tool: &ManifestTool,
    supplied: &Map<String, Value>,
) -> Result<Value, String> {
    let declared: HashMap<&str, &ManifestArgument> = tool
        .args
        .iter()
        .map(|arg| (arg.name.as_str(), arg))
        .collect();
    if let Some(unknown) = supplied
        .keys()
        .filter(|name| !declared.contains_key(name.as_str()))
        .min()
    {
        return Err(format!("unknown argument `{unknown}`"));
    }
    let mut resolved = supplied.clone();
    for arg in &tool.args {
        let active = arg
            .required_when
            .as_ref()
            .is_none_or(|condition| condition_matches(condition, &resolved));
        if !active {
            if resolved.contains_key(&arg.name) {
                return Err(format!(
                    "`{}` is not accepted when its condition is false",
                    arg.name
                ));
            }
            continue;
        }
        if !resolved.contains_key(&arg.name) {
            if let Some(default) = arg.default.as_ref() {
                resolved.insert(arg.name.clone(), default.clone());
            } else if arg.required || arg.required_when.is_some() {
                return Err(format!("missing required argument `{}`", arg.name));
            } else {
                continue;
            }
        }
        let value = resolved
            .get(&arg.name)
            .expect("resolved argument inserted or supplied");
        if arg.repeatable {
            let values = value
                .as_array()
                .ok_or_else(|| format!("`{}` must be an array", arg.name))?;
            for value in values {
                validate_call_scalar(arg, value)?;
            }
        } else {
            validate_call_scalar(arg, value)?;
        }
    }
    Ok(Value::Object(resolved))
}

fn validate_call_scalar(arg: &ManifestArgument, value: &Value) -> Result<(), String> {
    validate_scalar(&arg.name, arg.kind, value)?;
    if !arg.choices.is_empty() && !arg.choices.iter().any(|choice| values_equal(choice, value)) {
        return Err(format!("`{}` is not one of its allowed values", arg.name));
    }
    Ok(())
}

fn validate_scalar(name: &str, kind: ArgumentKind, value: &Value) -> Result<(), String> {
    let valid = match kind {
        ArgumentKind::String => value.is_string(),
        ArgumentKind::Bool => value.is_boolean(),
        ArgumentKind::Number => value.is_number(),
        ArgumentKind::Integer => value.as_number().is_some_and(number_is_integer),
    };
    if valid {
        return Ok(());
    }
    let expected = match kind {
        ArgumentKind::String => "a string",
        ArgumentKind::Bool => "a boolean",
        ArgumentKind::Number => "a number",
        ArgumentKind::Integer => "an integer",
    };
    Err(format!("`{name}` must be {expected}"))
}

fn condition_matches(condition: &Condition, values: &Map<String, Value>) -> bool {
    let value = values.get(&condition.arg);
    match condition.kind {
        ConditionKind::Present => value.is_some(),
        ConditionKind::Equals => value
            .zip(condition.value.as_ref())
            .is_some_and(|(left, right)| values_equal(left, right)),
        ConditionKind::NotEquals => value
            .zip(condition.value.as_ref())
            .is_some_and(|(left, right)| !values_equal(left, right)),
    }
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &str,
    message: &str,
) -> Result<&'a str, AppError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| AppError::Manifest(message.into()))
}

fn optional_bool(
    object: &Map<String, Value>,
    field: &str,
    tool_name: &str,
    arg_name: &str,
) -> Result<bool, AppError> {
    match object.get(field) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(AppError::Manifest(format!(
            "tool `{tool_name}` arg `{arg_name}` {field} must be boolean"
        ))),
        None => Ok(false),
    }
}

fn localized_english(value: Option<&Value>, field: &str) -> Result<String, AppError> {
    value
        .and_then(Value::as_object)
        .and_then(|localized| localized.get("en"))
        .and_then(Value::as_str)
        .filter(|english| !english.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| AppError::Manifest(format!("`{field}` requires non-empty English text")))
}

fn json_type(kind: ArgumentKind) -> &'static str {
    match kind {
        ArgumentKind::String => "string",
        ArgumentKind::Number => "number",
        ArgumentKind::Integer => "integer",
        ArgumentKind::Bool => "boolean",
    }
}

fn integer_equals(value: &Value, expected: i64) -> bool {
    value
        .as_number()
        .and_then(number_key)
        .is_some_and(|(digits, exponent)| digits == expected.to_string() && exponent.is_zero())
}

pub(crate) fn number_is_integer(number: &Number) -> bool {
    number_key(number).is_some_and(|(digits, exponent)| digits == "0" || exponent >= BigInt::zero())
}

fn values_equal(left: &Value, right: &Value) -> bool {
    match (left.as_number(), right.as_number()) {
        (Some(left), Some(right)) => number_key(left) == number_key(right),
        _ => left == right,
    }
}

fn number_key(number: &Number) -> Option<(String, BigInt)> {
    let lexeme = number.as_str();
    let (mantissa, exponent) = match lexeme.find(['e', 'E']) {
        Some(index) => (
            &lexeme[..index],
            BigInt::from_str(&lexeme[index + 1..]).ok()?,
        ),
        None => (lexeme, BigInt::zero()),
    };
    let negative = mantissa.starts_with('-');
    let unsigned = mantissa.strip_prefix('-').unwrap_or(mantissa);
    let (integer, fraction) = match unsigned.split_once('.') {
        Some(parts) => parts,
        None => (unsigned, ""),
    };
    let mut digits = format!("{integer}{fraction}");
    let leading = digits.len() - digits.trim_start_matches('0').len();
    digits.drain(..leading);
    if digits.is_empty() {
        return Some(("0".into(), BigInt::zero()));
    }
    let mut decimal_exponent = exponent - BigInt::from(fraction.len());
    while digits.ends_with('0') {
        digits.pop();
        decimal_exponent += 1;
    }
    if negative {
        digits.insert(0, '-');
    }
    Some((digits, decimal_exponent))
}
