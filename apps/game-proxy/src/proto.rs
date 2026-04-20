pub mod myserver {
    pub mod game {
        include!(concat!(env!("OUT_DIR"), "/myserver.game.rs"));
    }
}
