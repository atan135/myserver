fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mode = args
        .windows(2)
        .find_map(|pair| (pair[0] == "--mode").then(|| pair[1].as_str()));
    let result = match mode {
        Some("online") => lockstep_client::online::run_cli(args)
            .map(|report| report.to_string())
            .map_err(|error| error.to_string()),
        _ => lockstep_client::offline::run_cli(args)
            .map(|report| report.to_string())
            .map_err(|error| error.to_string()),
    };

    match result {
        Ok(report) => println!("{report}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
