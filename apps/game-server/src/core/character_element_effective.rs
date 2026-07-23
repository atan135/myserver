use serde::{Deserialize, Serialize};

use crate::business::character_element::{
    CharacterElementSnapshot, CharacterElements, ElementDeltas, ElementValues,
};
use crate::core::character_discipline::CharacterDiscipline;
use crate::core::inventory::item::ItemElementValues;
use crate::core::inventory::player_data::PlayerData;
use crate::core::system::combat::components::{Health, Stats};
use crate::csv_code::itemtable::ItemTable;

const BASE_AFFINITY: i32 = 2_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementModifier {
    pub affinity: ElementDeltas,
    pub mastery: ElementDeltas,
}

impl ElementModifier {
    pub const fn new(affinity: ElementDeltas, mastery: ElementDeltas) -> Self {
        Self { affinity, mastery }
    }

    pub const fn zero() -> Self {
        Self::new(ElementDeltas::zero(), ElementDeltas::zero())
    }

    pub const fn affinity(earth: i32, fire: i32, water: i32, wind: i32) -> Self {
        Self::new(
            ElementDeltas::new(earth, fire, water, wind),
            ElementDeltas::zero(),
        )
    }

    pub const fn mastery(earth: i32, fire: i32, water: i32, wind: i32) -> Self {
        Self::new(
            ElementDeltas::zero(),
            ElementDeltas::new(earth, fire, water, wind),
        )
    }

    pub fn from_item_elements(item_elements: ItemElementValues) -> Self {
        Self::mastery(
            item_elements.earth,
            item_elements.fire,
            item_elements.water,
            item_elements.wind,
        )
    }

    pub fn from_elements_delta(before: &CharacterElements, after: &CharacterElements) -> Self {
        Self::new(
            delta_between(before.affinity, after.affinity),
            delta_between(before.mastery, after.mastery),
        )
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self::new(
            add_deltas(self.affinity, other.affinity),
            add_deltas(self.mastery, other.mastery),
        )
    }
}

impl Default for ElementModifier {
    fn default() -> Self {
        Self::zero()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisciplineElementModifier {
    pub discipline_id: String,
    pub modifier: ElementModifier,
}

impl DisciplineElementModifier {
    pub fn new(discipline_id: impl Into<String>, modifier: ElementModifier) -> Self {
        Self {
            discipline_id: discipline_id.into(),
            modifier,
        }
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveElementsRequest {
    /// Permanent state is supplied as the business module's read-only snapshot.
    pub base_elements: CharacterElementSnapshot,
    pub disciplines: Vec<CharacterDiscipline>,
    pub discipline_modifiers: Vec<DisciplineElementModifier>,
    pub equipped_item_elements: ItemElementValues,
    pub buff_modifier: ElementModifier,
    pub scene_context_modifier: ElementModifier,
    pub temporary_system_modifier: ElementModifier,
}

impl EffectiveElementsRequest {
    pub fn new(base_elements: CharacterElementSnapshot) -> Self {
        Self {
            base_elements,
            disciplines: Vec::new(),
            discipline_modifiers: Vec::new(),
            equipped_item_elements: ItemElementValues::zero(),
            buff_modifier: ElementModifier::zero(),
            scene_context_modifier: ElementModifier::zero(),
            temporary_system_modifier: ElementModifier::zero(),
        }
    }

    pub fn with_disciplines(mut self, disciplines: Vec<CharacterDiscipline>) -> Self {
        self.disciplines = disciplines;
        self
    }

    pub fn with_discipline_modifiers(mut self, modifiers: Vec<DisciplineElementModifier>) -> Self {
        self.discipline_modifiers = modifiers;
        self
    }

    pub fn with_equipped_item_elements(mut self, item_elements: ItemElementValues) -> Self {
        self.equipped_item_elements = item_elements;
        self
    }

    pub fn with_player_data(mut self, player_data: &PlayerData, item_table: &ItemTable) -> Self {
        self.equipped_item_elements = player_data.effective_item_elements(item_table);
        self
    }

    pub fn with_buff_modifier(mut self, modifier: ElementModifier) -> Self {
        self.buff_modifier = modifier;
        self
    }

    pub fn with_scene_context_modifier(mut self, modifier: ElementModifier) -> Self {
        self.scene_context_modifier = modifier;
        self
    }

    pub fn with_temporary_system_modifier(mut self, modifier: ElementModifier) -> Self {
        self.temporary_system_modifier = modifier;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveElementSources {
    pub base_elements: CharacterElementSnapshot,
    pub active_discipline_ids: Vec<String>,
    pub discipline_modifier: ElementModifier,
    pub equipped_item_elements: ItemElementValues,
    pub equipped_item_modifier: ElementModifier,
    pub buff_modifier: ElementModifier,
    pub scene_context_modifier: ElementModifier,
    pub temporary_system_modifier: ElementModifier,
    pub total_modifier: ElementModifier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveElementsResult {
    pub character_id: String,
    pub elements: CharacterElements,
    pub sources: EffectiveElementSources,
}

impl EffectiveElementsResult {
    pub fn combat_attributes(&self) -> EffectiveCombatAttributes {
        derive_combat_attributes(&self.elements)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EffectiveCombatAttributes {
    pub health: Health,
    pub stats: Stats,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct EffectiveElementsService;

impl EffectiveElementsService {
    pub fn calculate(&self, request: EffectiveElementsRequest) -> EffectiveElementsResult {
        calculate_effective_elements(request)
    }
}

pub fn calculate_effective_elements(request: EffectiveElementsRequest) -> EffectiveElementsResult {
    let active_discipline_ids = request
        .disciplines
        .iter()
        .filter(|discipline| discipline.active)
        .map(|discipline| discipline.discipline_id.clone())
        .collect::<Vec<_>>();
    let discipline_modifier =
        active_discipline_modifier(&active_discipline_ids, &request.discipline_modifiers);
    let equipped_item_modifier =
        ElementModifier::from_item_elements(request.equipped_item_elements);

    let total_modifier = discipline_modifier
        .saturating_add(equipped_item_modifier)
        .saturating_add(request.buff_modifier)
        .saturating_add(request.scene_context_modifier)
        .saturating_add(request.temporary_system_modifier);

    let mut elements = effective_elements_from_snapshot(&request.base_elements);
    apply_modifier(&mut elements, total_modifier);

    EffectiveElementsResult {
        character_id: request.base_elements.character_id().to_string(),
        elements,
        sources: EffectiveElementSources {
            base_elements: request.base_elements,
            active_discipline_ids,
            discipline_modifier,
            equipped_item_elements: request.equipped_item_elements,
            equipped_item_modifier,
            buff_modifier: request.buff_modifier,
            scene_context_modifier: request.scene_context_modifier,
            temporary_system_modifier: request.temporary_system_modifier,
            total_modifier,
        },
    }
}

fn effective_elements_from_snapshot(snapshot: &CharacterElementSnapshot) -> CharacterElements {
    let affinity = snapshot.affinity();
    let mastery = snapshot.mastery();
    CharacterElements {
        character_id: snapshot.character_id().to_string(),
        affinity: ElementValues::new(
            affinity.earth(),
            affinity.fire(),
            affinity.water(),
            affinity.wind(),
        ),
        mastery: ElementValues::new(
            mastery.earth(),
            mastery.fire(),
            mastery.water(),
            mastery.wind(),
        ),
    }
}

pub fn derive_combat_attributes(elements: &CharacterElements) -> EffectiveCombatAttributes {
    let earth = i64::from(elements.mastery.earth.max(0));
    let fire = i64::from(elements.mastery.fire.max(0));
    let water = i64::from(elements.mastery.water.max(0));
    let wind = i64::from(elements.mastery.wind.max(0));

    let earth_bias = i64::from(affinity_bias(elements.affinity.earth));
    let fire_bias = i64::from(affinity_bias(elements.affinity.fire));
    let water_bias = i64::from(affinity_bias(elements.affinity.water));
    let wind_bias = i64::from(affinity_bias(elements.affinity.wind));

    let max_hp = clamp_i64_to_i32(120 + earth / 2 + water / 4 + earth_bias, 1, i32::MAX);
    let attack = clamp_i64_to_i32(20 + fire / 5 + wind / 10 + fire_bias, 0, i32::MAX);
    let defense = clamp_i64_to_i32(10 + earth / 5 + water / 8 + water_bias, 0, i32::MAX);
    let speed = clamp_i64_to_i32(120 + wind / 20 + wind_bias, 1, i32::MAX);
    let crit_rate_bps = clamp_i64_to_u16(500 + fire / 2 + wind / 3 + fire_bias * 10, 0, 9_000);
    let crit_damage_bps = clamp_i64_to_u16(5_000 + fire / 4, 1_000, 30_000);

    EffectiveCombatAttributes {
        health: Health::new(max_hp),
        stats: Stats {
            attack,
            defense,
            speed,
            crit_rate_bps,
            crit_damage_bps,
        },
    }
}

fn active_discipline_modifier(
    active_discipline_ids: &[String],
    modifiers: &[DisciplineElementModifier],
) -> ElementModifier {
    modifiers
        .iter()
        .filter(|modifier| {
            active_discipline_ids
                .iter()
                .any(|discipline_id| discipline_id.eq_ignore_ascii_case(&modifier.discipline_id))
        })
        .fold(ElementModifier::zero(), |acc, modifier| {
            acc.saturating_add(modifier.modifier)
        })
}

fn apply_modifier(elements: &mut CharacterElements, modifier: ElementModifier) {
    elements.affinity = apply_delta_clamped(elements.affinity, modifier.affinity);
    elements.mastery = apply_delta_clamped(elements.mastery, modifier.mastery);
}

fn apply_delta_clamped(values: ElementValues, delta: ElementDeltas) -> ElementValues {
    ElementValues::new(
        add_clamped(values.earth, delta.earth),
        add_clamped(values.fire, delta.fire),
        add_clamped(values.water, delta.water),
        add_clamped(values.wind, delta.wind),
    )
}

fn add_clamped(current: i32, delta: i32) -> i32 {
    let value = i64::from(current) + i64::from(delta);
    if value <= 0 {
        return 0;
    }
    if value > i64::from(i32::MAX) {
        return i32::MAX;
    }
    value as i32
}

fn delta_between(before: ElementValues, after: ElementValues) -> ElementDeltas {
    ElementDeltas::new(
        after.earth.saturating_sub(before.earth),
        after.fire.saturating_sub(before.fire),
        after.water.saturating_sub(before.water),
        after.wind.saturating_sub(before.wind),
    )
}

fn add_deltas(left: ElementDeltas, right: ElementDeltas) -> ElementDeltas {
    ElementDeltas::new(
        left.earth.saturating_add(right.earth),
        left.fire.saturating_add(right.fire),
        left.water.saturating_add(right.water),
        left.wind.saturating_add(right.wind),
    )
}

fn affinity_bias(value: i32) -> i32 {
    value.saturating_sub(BASE_AFFINITY) / 250
}

fn clamp_i64_to_i32(value: i64, min: i32, max: i32) -> i32 {
    value.clamp(i64::from(min), i64::from(max)) as i32
}

fn clamp_i64_to_u16(value: i64, min: u16, max: u16) -> u16 {
    value.clamp(i64::from(min), i64::from(max)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::inventory::{EquipSlot, Item};
    use crate::csv_code::itemtable::{ItemTable, ItemTableRow};
    use std::collections::HashMap;

    fn base_elements() -> CharacterElementSnapshot {
        CharacterElementSnapshot::new(
            "chr_0000000000001",
            crate::business::character_element::ElementSnapshot::new(2500, 2500, 2500, 2500),
            crate::business::character_element::ElementSnapshot::new(10, 20, 30, 40),
        )
    }

    fn discipline(discipline_id: &str, active: bool) -> CharacterDiscipline {
        CharacterDiscipline {
            character_id: "chr_0000000000001".to_string(),
            discipline_id: discipline_id.to_string(),
            points: 0,
            tier: "novice".to_string(),
            active,
            learned_at: "now".to_string(),
            updated_at: "now".to_string(),
        }
    }

    fn item_table() -> ItemTable {
        let rows = vec![ItemTableRow {
            id: 1002,
            templateelementfire: 80,
            ..ItemTableRow::default()
        }];
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.id, index))
            .collect();
        ItemTable {
            string_pool: HashMap::new(),
            rows,
            by_id,
        }
    }

    #[test]
    fn effective_elements_combine_active_discipline_equipment_buff_scene_and_system_sources() {
        let base = base_elements();
        let mut player_data = PlayerData::new(base.character_id().to_string());
        let mut item = Item::new(7, 1002, 1, false);
        item.growth_elements = ItemElementValues::new(1, 2, 3, 4);
        item.runtime_elements = ItemElementValues::new(0, 5, 0, 0);
        player_data
            .equipment
            .equip(EquipSlot::Weapon, item)
            .unwrap();

        let result = EffectiveElementsService.calculate(
            EffectiveElementsRequest::new(base.clone())
                .with_disciplines(vec![
                    discipline("forging", true),
                    discipline("fire_art", false),
                ])
                .with_discipline_modifiers(vec![
                    DisciplineElementModifier::new("forging", ElementModifier::mastery(0, 7, 0, 0)),
                    DisciplineElementModifier::new(
                        "fire_art",
                        ElementModifier::mastery(0, 100, 0, 0),
                    ),
                ])
                .with_player_data(&player_data, &item_table())
                .with_buff_modifier(ElementModifier::mastery(0, 5, 0, 0))
                .with_scene_context_modifier(ElementModifier::new(
                    ElementDeltas::new(0, 120, 0, 0),
                    ElementDeltas::new(0, -2, 0, 3),
                ))
                .with_temporary_system_modifier(ElementModifier::mastery(1, 1, 1, 1)),
        );

        assert_eq!(base.mastery().fire(), 20);
        assert_eq!(result.sources.active_discipline_ids, vec!["forging"]);
        assert_eq!(result.sources.equipped_item_elements.fire, 87);
        assert_eq!(result.elements.affinity.fire, 2620);
        assert_eq!(result.elements.mastery.earth, 12);
        assert_eq!(result.elements.mastery.fire, 118);
        assert_eq!(result.elements.mastery.water, 34);
        assert_eq!(result.elements.mastery.wind, 48);
    }

    #[test]
    fn switching_active_discipline_changes_effective_elements_without_level_input() {
        let base = base_elements();
        let modifiers = vec![
            DisciplineElementModifier::new("forging", ElementModifier::mastery(10, 0, 0, 0)),
            DisciplineElementModifier::new("fire_art", ElementModifier::mastery(0, 20, 0, 0)),
        ];

        let forging = calculate_effective_elements(
            EffectiveElementsRequest::new(base.clone())
                .with_disciplines(vec![
                    discipline("forging", true),
                    discipline("fire_art", false),
                ])
                .with_discipline_modifiers(modifiers.clone()),
        );
        let fire_art = calculate_effective_elements(
            EffectiveElementsRequest::new(base)
                .with_disciplines(vec![
                    discipline("forging", false),
                    discipline("fire_art", true),
                ])
                .with_discipline_modifiers(modifiers),
        );

        assert_eq!(forging.elements.mastery.earth, 20);
        assert_eq!(forging.elements.mastery.fire, 20);
        assert_eq!(fire_art.elements.mastery.earth, 10);
        assert_eq!(fire_art.elements.mastery.fire, 40);
    }

    #[test]
    fn scene_effective_elements_can_be_converted_to_temporary_modifier() {
        let base = base_elements();
        let scene_effective = CharacterElements {
            character_id: base.character_id().to_string(),
            affinity: ElementValues::new(2500, 2620, 2500, 2500),
            mastery: ElementValues::new(10, 18, 30, 43),
        };

        let modifier = ElementModifier::from_elements_delta(
            &effective_elements_from_snapshot(&base),
            &scene_effective,
        );
        let result = calculate_effective_elements(
            EffectiveElementsRequest::new(base).with_scene_context_modifier(modifier),
        );

        assert_eq!(result.elements.affinity.fire, 2620);
        assert_eq!(result.elements.mastery.fire, 18);
        assert_eq!(result.elements.mastery.wind, 43);
    }

    #[test]
    fn combat_attributes_are_derived_from_effective_mastery_and_affinity_without_level() {
        let base = base_elements();
        let weak = calculate_effective_elements(EffectiveElementsRequest::new(base.clone()));
        let strong = calculate_effective_elements(
            EffectiveElementsRequest::new(base)
                .with_buff_modifier(ElementModifier::mastery(100, 200, 50, 80))
                .with_scene_context_modifier(ElementModifier::affinity(250, 500, 0, 250)),
        );

        let weak_attrs = weak.combat_attributes();
        let strong_attrs = strong.combat_attributes();

        assert!(strong_attrs.health.max > weak_attrs.health.max);
        assert!(strong_attrs.stats.attack > weak_attrs.stats.attack);
        assert!(strong_attrs.stats.defense > weak_attrs.stats.defense);
        assert!(strong_attrs.stats.speed > weak_attrs.stats.speed);
    }

    #[test]
    fn temporary_negative_modifiers_do_not_underflow_effective_elements() {
        let result = calculate_effective_elements(
            EffectiveElementsRequest::new(base_elements())
                .with_buff_modifier(ElementModifier::mastery(0, -500, 0, 0)),
        );

        assert_eq!(result.elements.mastery.fire, 0);
    }
}
