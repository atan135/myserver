use crate::core::config_table::CsvLoadError;
use crate::csv_code::movementconfig::MovementConfig;

pub const MAX_MOVEMENT_SPEED_METRES_PER_SECOND: f32 = 100.0;

pub fn valid_movement_speed(speed: f32) -> bool {
    speed.is_finite() && speed > 0.0 && speed <= MAX_MOVEMENT_SPEED_METRES_PER_SECOND
}

pub fn validate_movement_config(table: &MovementConfig) -> Result<f32, CsvLoadError> {
    if table.rows.len() != 1 || table.rows[0].id != 1 {
        return Err(CsvLoadError::InvalidRow(
            "MovementConfig.csv Id: exactly one row with Id=1 is required".into(),
        ));
    }
    let speed = table.rows[0].basespeedmetrespersecond;
    if !valid_movement_speed(speed) {
        return Err(CsvLoadError::InvalidRow(
            "MovementConfig.csv BaseSpeedMetresPerSecond: expected finite 0 < speed <= 100".into(),
        ));
    }
    Ok(speed)
}
