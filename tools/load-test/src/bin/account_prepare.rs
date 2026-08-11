use std::env;
use std::path::Path;
use std::process::ExitCode;

use loadtest_core::config::load_config;

fn main() -> ExitCode {
    match execute(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("account-prepare: {error}");
            ExitCode::from(2)
        }
    }
}

fn execute(arguments: Vec<String>) -> Result<(), String> {
    let mut arguments = arguments.into_iter();
    let command = arguments
        .next()
        .ok_or("usage: account-prepare <plan|apply|verify|export> --config <file>")?;
    let flag = arguments.next().ok_or("--config is required")?;
    if flag != "--config" {
        return Err("--config is required".into());
    }
    let config_path = arguments.next().ok_or("--config requires a path")?;
    if arguments.next().is_some() {
        return Err("unknown account-prepare argument".into());
    }
    let config = load_config(Path::new(&config_path), None).map_err(|error| error.to_string())?;
    match command.as_str() {
        "plan" => {
            let plan_root = Path::new(&config.prepare_reports_root).join("plans");
            std::fs::create_dir_all(&plan_root).map_err(|error| error.to_string())?;
            let plan = serde_json::json!({"schema_version": loadtest_core::SCHEMA_VERSION, "environment": config.environment.name, "scenario": config.scenario.name, "prepare_result_root": config.prepare_reports_root, "writes_planned": 0, "status": "stage-one-skeleton"});
            let path = plan_root.join("plan.json");
            std::fs::write(&path, serde_json::to_vec_pretty(&plan).unwrap())
                .map_err(|error| error.to_string())?;
            println!(
                "account plan contains no credentials and no business writes: {}",
                path.display()
            );
            Ok(())
        }
        "apply" | "verify" | "export" => Err(format!(
            "account-prepare {command} is intentionally unavailable until stage two; no accounts or services were touched"
        )),
        _ => Err("usage: account-prepare <plan|apply|verify|export> --config <file>".into()),
    }
}
