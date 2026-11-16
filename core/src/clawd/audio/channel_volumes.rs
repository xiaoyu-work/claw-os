use super::*;

pub(super) fn parse(value: &str) -> Result<Vec<f64>, String> {
    let channels: Vec<f64> = value
        .split(':')
        .map(|value| {
            value
                .parse::<f64>()
                .map_err(|_| "invalid channel volume".to_owned())
        })
        .collect::<Result<_, _>>()?;
    if channels.is_empty()
        || channels.len() > 32
        || channels
            .iter()
            .any(|value| !value.is_finite() || !(0.0..=3.375).contains(value))
    {
        return Err("output channel volumes require 1 to 32 finite values in 0..3.375".into());
    }
    Ok(channels)
}

pub(super) async fn apply(
    environment: &AudioEnvironment,
    target: &str,
    value: &str,
) -> Result<Value, String> {
    let id = parse_id(target)?;
    let requested = parse(value)?;
    let before = require_media_class(environment, id, Direction::Output.media_class()).await?;
    let current = channels(&before)?;
    if current.len() != requested.len() {
        return Err("output channel count changed; refresh before retrying".into());
    }
    let guard = ObjectGuard::from_object(&before)?;
    let program = ["/usr/bin/pw-cli", "/bin/pw-cli"]
        .into_iter()
        .find(|path| std::path::Path::new(path).is_file())
        .ok_or("OS PipeWire parameter helper is unavailable")?;
    let argument = format!(
        "{{ channelVolumes: [ {} ] }}",
        requested
            .iter()
            .map(|value| format!("{value:.6}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    guard.verify(environment).await?;
    run_user_tool(
        program,
        &["set-param", &id.to_string(), "Props", &argument],
        environment,
        ChildPolicy,
    )
    .await?;
    guard
        .verify(environment)
        .await
        .map_err(|_| "output identity changed after dispatch; refresh before retrying")?;
    let after = require_media_class(environment, id, Direction::Output.media_class())
        .await
        .map_err(|_| "output volume may have changed; readback failed; refresh before retrying")?;
    let observed = channels(&after).map_err(|_| {
        "output volume may have changed; channel readback failed; refresh before retrying"
    })?;
    if observed.len() != requested.len()
        || observed
            .iter()
            .zip(&requested)
            .any(|(actual, wanted)| (actual - wanted).abs() > 0.0001)
    {
        return Err(
            "output volume was dispatched but readback differs; refresh before retrying".into(),
        );
    }
    Ok(
        json!({"action":"output-channel-volumes","target":target,"action_applied":true,"before":before,"after":after}),
    )
}

fn channels(object: &Value) -> Result<Vec<f64>, String> {
    let values = object["parameters"]["Props"]
        .as_array()
        .and_then(|props| {
            props
                .iter()
                .find_map(|prop| prop["channelVolumes"].as_array())
        })
        .ok_or("output channel volumes are unavailable")?;
    let values = values
        .iter()
        .map(|value| {
            value
                .as_f64()
                .ok_or_else(|| "invalid output channel volume".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if values.is_empty() || values.len() > 32 || values.iter().any(|value| !value.is_finite()) {
        return Err("invalid output channel volume observation".into());
    }
    Ok(values)
}
