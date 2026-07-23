/// Temporary compatibility forwarding for the progress handler. The actual
/// permanent element protocol mapping lives in `gameservice::character_element`.
/// Remove this module after stage 7 moves the remaining caller.
pub(crate) use crate::gameservice::character_element::queue_legacy_character_element_push as queue_character_element_push;
