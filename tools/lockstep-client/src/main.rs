fn main() {
    match lockstep_client::offline::run_cli(std::env::args().skip(1)) {
        Ok(report) => println!("{report}"),
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
