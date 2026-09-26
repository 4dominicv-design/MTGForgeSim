//! Compact binary board-state snapshot for URL-shareable pile scenarios.
//!
//! Cards are identified as `(set_index: u16, collector_number: u16)` — 4 bytes
//! per card, mapping to real Magic set + collector number pairs via a
//! [`CardRegistry`].
//!
//! # Wire format (version 2)
//!
//! `pile_slot` is a u8 in 0..=5: 0 = not in pile, 1 = top of pile, 5 = bottom.
//! It's packed into 3 bits of each card/permanent flag byte.
//!
//! ```text
//! HEADER (10 bytes):
//!   [0]     version (u8) = 2
//!   [1]     turn (u8)
//!   [2]     stage (u8: 0=Early, 1=Mid, 2=Late)
//!   [3]     flags: bit0=on_play  bit1=us_land_drop  bit2=opp_land_drop
//!   [4‑5]   us_life   (i16 LE)
//!   [6‑7]   opp_life  (i16 LE)
//!   [8‑9]   life_before_dd (i16 LE; i16::MIN = None)
//!
//! STACK (shared zone):
//!   [1]     count (u8)
//!   Per entry — CARD (5 bytes):
//!       [2] set_index  (u16 LE)
//!       [2] collector  (u16 LE)
//!       [1] flags: bits0‑2=pile_slot  bit3=known
//!
//! PER PLAYER (us first, then opp):
//!   [1]     deck_name_len (u8)
//!   [N]     deck_name     (UTF-8)
//!
//!   2× PERMANENT ZONE  (lands, permanents) — 7 bytes each:
//!       [2] set_index  (u16 LE)
//!       [2] collector  (u16 LE)
//!       [1] flags: bit0=tapped  bit1=flipped  bits2‑4=pile_slot
//!       [1] counters   (u8)
//!       [1] loyalty    (u8)
//!
//!   4× CARD ZONE  (hand, library, graveyard, exile) — 5 bytes each:
//!       [2] set_index  (u16 LE)
//!       [2] collector  (u16 LE)
//!       [1] flags: bits0‑2=pile_slot  bit3=known
//!
//!   [1]     hand_hidden (u8)
//! ```

use std::collections::HashMap;
use std::fmt;

// ── Card identity ────────────────────────────────────────────────────────────

/// A card identified by set + collector number.  Wire: two LE u16s (4 bytes).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CardId {
    pub set_index: u16,
    pub collector_number: u16,
}

impl CardId {
    pub const fn new(set_index: u16, collector_number: u16) -> Self {
        Self { set_index, collector_number }
    }
}

// ── Snapshot types ───────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct BoardSnapshot {
    pub turn: u8,
    pub stage: Stage,
    pub on_play: bool,
    pub life_before_dd: Option<i16>,
    pub stack: Vec<CardEntry>,
    pub us: PlayerSnapshot,
    pub opp: PlayerSnapshot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    Early = 0,
    Mid = 1,
    Late = 2,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlayerSnapshot {
    pub deck_name: String,
    pub life: i16,
    pub land_drop_available: bool,
    pub lands: Vec<PermanentEntry>,
    pub permanents: Vec<PermanentEntry>,
    pub hand: Vec<CardEntry>,
    pub library: Vec<CardEntry>,
    pub graveyard: Vec<CardEntry>,
    pub exile: Vec<CardEntry>,
    pub hand_hidden: u8,
}

/// A card on the battlefield — carries tap / counter / loyalty state.
#[derive(Clone, Debug, PartialEq)]
pub struct PermanentEntry {
    pub id: CardId,
    pub tapped: bool,
    pub flipped: bool,
    /// 0 = not in pile; 1..=5 = slot (1 = top of pile, 5 = bottom).
    pub pile_slot: u8,
    pub counters: u8,
    pub loyalty: u8,
}

/// A card in any non-battlefield zone.
#[derive(Clone, Debug, PartialEq)]
pub struct CardEntry {
    pub id: CardId,
    /// 0 = not in pile; 1..=5 = slot (1 = top of pile, 5 = bottom).
    pub pile_slot: u8,
    /// True if this card is known/revealed (relevant for opponent's hand).
    pub known: bool,
}

// ── Card registry ────────────────────────────────────────────────────────────

/// Two-way lookup between card names and `CardId` (set + collector number).
///
/// Built from `(name, set_code, collector_number)` entries.  Set codes are
/// sorted alphabetically to produce stable `set_index` values.
pub struct CardRegistry {
    set_to_index: HashMap<String, u16>,
    index_to_set: Vec<String>,
    name_to_id: HashMap<String, CardId>,
    id_to_name: HashMap<CardId, String>,
}

impl CardRegistry {
    /// Build from `(card_name, set_code, collector_number)` triples.
    ///
    /// Set codes are sorted alphabetically; their position becomes the u16
    /// `set_index` used in the wire format.
    pub fn from_entries(entries: &[(&str, &str, u16)]) -> Self {
        // Stable set-code ordering: sorted alphabetically.
        let mut set_codes: Vec<String> = entries
            .iter()
            .map(|(_, set, _)| set.to_string())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        set_codes.sort();

        let set_to_index: HashMap<String, u16> = set_codes
            .iter()
            .enumerate()
            .map(|(i, s)| (s.clone(), i as u16))
            .collect();

        let mut name_to_id = HashMap::new();
        let mut id_to_name = HashMap::new();

        for &(name, set_code, collector_num) in entries {
            let set_idx = set_to_index[set_code];
            let cid = CardId::new(set_idx, collector_num);
            name_to_id.insert(name.to_string(), cid);
            id_to_name.insert(cid, name.to_string());
        }

        Self { set_to_index, index_to_set: set_codes, name_to_id, id_to_name }
    }

    pub fn name_to_id(&self, name: &str) -> Option<CardId> {
        self.name_to_id.get(name).copied()
    }

    pub fn id_to_name(&self, id: CardId) -> Option<&str> {
        self.id_to_name.get(&id).map(|s| s.as_str())
    }

    pub fn set_code(&self, set_index: u16) -> Option<&str> {
        self.index_to_set.get(set_index as usize).map(|s| s.as_str())
    }
}

// ── Conversion from ScenarioResult ───────────────────────────────────────────

use super::{ScenarioResult, PlayerResult, PermanentResult};

impl BoardSnapshot {
    /// Per-player snapshot accessor (mirrors `SimState::player`/`player_mut`).
    /// Used by the wasm pile-encode/decode path, which only touches our (Us) side.
    #[cfg(target_arch = "wasm32")]
    pub fn player(&self, who: crate::PlayerId) -> &PlayerSnapshot {
        match who { crate::PlayerId::Us => &self.us, crate::PlayerId::Opp => &self.opp }
    }
    #[cfg(target_arch = "wasm32")]
    pub fn player_mut(&mut self, who: crate::PlayerId) -> &mut PlayerSnapshot {
        match who { crate::PlayerId::Us => &mut self.us, crate::PlayerId::Opp => &mut self.opp }
    }

    /// Convert a `ScenarioResult` (name-based) into a compact `BoardSnapshot`
    /// (id-based).  All card names are resolved through the registry.
    pub fn from_result(
        r: &ScenarioResult,
        registry: &CardRegistry,
    ) -> Result<Self, SnapshotError> {
        let stage = match r.stage.as_str() {
            "Early" => Stage::Early,
            "Mid" => Stage::Mid,
            _ => Stage::Late,
        };

        let stack: Vec<CardEntry> = r.stack.iter()
            .map(|name| Ok(CardEntry {
                id: lookup(registry, name)?,
                pile_slot: 0,
                known: true,
            }))
            .collect::<Result<_, _>>()?;

        Ok(Self {
            turn: r.turn,
            stage,
            on_play: r.on_play,
            life_before_dd: r.life_before_dd.map(|l| l as i16),
            stack,
            us: PlayerSnapshot::from_player_result(&r.us, registry)?,
            opp: PlayerSnapshot::from_player_result(&r.opp, registry)?,
        })
    }

    /// Convert back to a `ScenarioResult`.  Logs are empty (they aren't part
    /// of the snapshot).
    pub fn to_result(&self, registry: &CardRegistry) -> ScenarioResult {
        let stage = match self.stage {
            Stage::Early => "Early",
            Stage::Mid => "Mid",
            Stage::Late => "Late",
        };

        ScenarioResult {
            turn: self.turn,
            stage: stage.to_string(),
            on_play: self.on_play,
            us: self.us.to_player_result(registry),
            opp: self.opp.to_player_result(registry),
            log: vec![],
            stack: self.stack.iter()
                .filter_map(|e| registry.id_to_name(e.id).map(|s| s.to_string()))
                .collect(),
            life_before_dd: self.life_before_dd.map(|l| l as i32),
            decision_log: vec![],
            text_summary: String::new(),
        }
    }
}

impl PlayerSnapshot {
    fn from_player_result(
        p: &PlayerResult,
        reg: &CardRegistry,
    ) -> Result<Self, SnapshotError> {
        let lands = p.lands.iter()
            .map(|pr| perm_entry(reg, pr))
            .collect::<Result<_, _>>()?;
        let permanents = p.permanents.iter()
            .map(|pr| perm_entry(reg, pr))
            .collect::<Result<_, _>>()?;
        let hand = p.hand.iter()
            .map(|cr| Ok(CardEntry {
                id: lookup(reg, &cr.name)?,
                pile_slot: 0,
                known: true,
            }))
            .collect::<Result<_, _>>()?;
        let library = p.library.iter()
            .map(|name| Ok(CardEntry {
                id: lookup(reg, name)?,
                pile_slot: 0,
                known: true,
            }))
            .collect::<Result<_, _>>()?;
        let graveyard = p.graveyard.iter()
            .map(|name| Ok(CardEntry {
                id: lookup(reg, name)?,
                pile_slot: 0,
                known: false,
            }))
            .collect::<Result<_, _>>()?;
        let exile = p.exile.iter()
            .map(|name| Ok(CardEntry {
                id: lookup(reg, name)?,
                pile_slot: 0,
                known: false,
            }))
            .collect::<Result<_, _>>()?;

        Ok(Self {
            deck_name: p.deck_name.clone(),
            life: p.life as i16,
            land_drop_available: p.land_drop_available,
            lands,
            permanents,
            hand,
            library,
            graveyard,
            exile,
            hand_hidden: p.hand_hidden as u8,
        })
    }

    fn to_player_result(&self, reg: &CardRegistry) -> PlayerResult {
        let resolve = |id: CardId| -> String {
            reg.id_to_name(id).unwrap_or("???").to_string()
        };

        PlayerResult {
            deck_name: self.deck_name.clone(),
            life: self.life as i32,
            lands: self.lands.iter().map(|e| PermanentResult {
                name: resolve(e.id),
                tapped: e.tapped,
                counters: e.counters as i32,
                loyalty: e.loyalty as i32,
                flipped: e.flipped,
            }).collect(),
            permanents: self.permanents.iter().map(|e| PermanentResult {
                name: resolve(e.id),
                tapped: e.tapped,
                counters: e.counters as i32,
                loyalty: e.loyalty as i32,
                flipped: e.flipped,
            }).collect(),
            hand: self.hand.iter()
                .map(|e| super::CardResult { name: resolve(e.id) })
                .collect(),
            hand_hidden: self.hand_hidden as usize,
            land_drop_available: self.land_drop_available,
            library: self.library.iter().map(|e| resolve(e.id)).collect(),
            graveyard: self.graveyard.iter().map(|e| resolve(e.id)).collect(),
            exile: self.exile.iter().map(|e| resolve(e.id)).collect(),
        }
    }
}

fn lookup(reg: &CardRegistry, name: &str) -> Result<CardId, SnapshotError> {
    reg.name_to_id(name).ok_or_else(|| SnapshotError::UnknownCard(name.to_string()))
}

fn perm_entry(reg: &CardRegistry, pr: &PermanentResult) -> Result<PermanentEntry, SnapshotError> {
    Ok(PermanentEntry {
        id: lookup(reg, &pr.name)?,
        tapped: pr.tapped,
        flipped: pr.flipped,
        pile_slot: 0,
        counters: pr.counters as u8,
        loyalty: pr.loyalty as u8,
    })
}

// ── Binary encoding ──────────────────────────────────────────────────────────

const VERSION: u8 = 2;
const NONE_LIFE: i16 = i16::MIN;

pub fn encode(snap: &BoardSnapshot) -> Vec<u8> {
    let mut b = Vec::with_capacity(300);

    // Header (10 bytes).
    b.push(VERSION);
    b.push(snap.turn);
    b.push(snap.stage as u8);
    let mut flags: u8 = 0;
    if snap.on_play                  { flags |= 1; }
    if snap.us.land_drop_available   { flags |= 2; }
    if snap.opp.land_drop_available  { flags |= 4; }
    b.push(flags);
    b.extend_from_slice(&snap.us.life.to_le_bytes());
    b.extend_from_slice(&snap.opp.life.to_le_bytes());
    b.extend_from_slice(&snap.life_before_dd.unwrap_or(NONE_LIFE).to_le_bytes());

    // Stack.
    write_cards(&mut b, &snap.stack);

    // Players.
    write_player(&mut b, &snap.us);
    write_player(&mut b, &snap.opp);
    b
}

pub fn decode(data: &[u8]) -> Result<BoardSnapshot, SnapshotError> {
    let mut c = Cursor::new(data);

    let ver = c.u8()?;
    if ver != VERSION { return Err(SnapshotError::BadVersion(ver)); }

    let turn = c.u8()?;
    let stage = match c.u8()? {
        0 => Stage::Early,
        1 => Stage::Mid,
        2 => Stage::Late,
        x => return Err(SnapshotError::BadStage(x)),
    };
    let flags = c.u8()?;
    let on_play           = flags & 1 != 0;
    let us_land_drop      = flags & 2 != 0;
    let opp_land_drop     = flags & 4 != 0;

    let us_life  = c.i16()?;
    let opp_life = c.i16()?;
    let lbdd     = c.i16()?;
    let life_before_dd = if lbdd == NONE_LIFE { None } else { Some(lbdd) };

    let stack = read_cards(&mut c)?;

    let mut us = read_player(&mut c)?;
    us.life = us_life;
    us.land_drop_available = us_land_drop;

    let mut opp = read_player(&mut c)?;
    opp.life = opp_life;
    opp.land_drop_available = opp_land_drop;

    Ok(BoardSnapshot { turn, stage, on_play, life_before_dd, stack, us, opp })
}

fn write_player(b: &mut Vec<u8>, p: &PlayerSnapshot) {
    let name = p.deck_name.as_bytes();
    b.push(name.len() as u8);
    b.extend_from_slice(name);

    // Permanent zones.
    for zone in [&p.lands, &p.permanents] {
        b.push(zone.len() as u8);
        for e in zone {
            b.extend_from_slice(&e.id.set_index.to_le_bytes());
            b.extend_from_slice(&e.id.collector_number.to_le_bytes());
            let mut f: u8 = 0;
            if e.tapped  { f |= 1; }
            if e.flipped { f |= 2; }
            f |= (e.pile_slot & 0x07) << 2;
            b.push(f);
            b.push(e.counters);
            b.push(e.loyalty);
        }
    }

    // Card zones.
    for zone in [&p.hand, &p.library, &p.graveyard, &p.exile] {
        write_cards(b, zone);
    }

    b.push(p.hand_hidden);
}

fn write_cards(b: &mut Vec<u8>, zone: &[CardEntry]) {
    b.push(zone.len() as u8);
    for e in zone {
        b.extend_from_slice(&e.id.set_index.to_le_bytes());
        b.extend_from_slice(&e.id.collector_number.to_le_bytes());
        let mut f: u8 = e.pile_slot & 0x07;
        if e.known { f |= 0x08; }
        b.push(f);
    }
}

fn read_player(c: &mut Cursor<'_>) -> Result<PlayerSnapshot, SnapshotError> {
    let name_len = c.u8()? as usize;
    let deck_name = String::from_utf8(c.bytes(name_len)?.to_vec())
        .map_err(|_| SnapshotError::BadUtf8)?;

    let lands      = read_permanents(c)?;
    let permanents = read_permanents(c)?;
    let hand       = read_cards(c)?;
    let library    = read_cards(c)?;
    let graveyard  = read_cards(c)?;
    let exile      = read_cards(c)?;
    let hand_hidden = c.u8()?;

    Ok(PlayerSnapshot {
        deck_name,
        life: 0,                    // filled in by caller from header
        land_drop_available: false,  // filled in by caller from header
        lands, permanents, hand, library, graveyard, exile, hand_hidden,
    })
}

fn read_permanents(c: &mut Cursor<'_>) -> Result<Vec<PermanentEntry>, SnapshotError> {
    let n = c.u8()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let set_index = c.u16()?;
        let coll = c.u16()?;
        let f = c.u8()?;
        v.push(PermanentEntry {
            id: CardId::new(set_index, coll),
            tapped:    f & 1 != 0,
            flipped:   f & 2 != 0,
            pile_slot: (f >> 2) & 0x07,
            counters:  c.u8()?,
            loyalty:   c.u8()?,
        });
    }
    Ok(v)
}

fn read_cards(c: &mut Cursor<'_>) -> Result<Vec<CardEntry>, SnapshotError> {
    let n = c.u8()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let set_index = c.u16()?;
        let coll = c.u16()?;
        let f = c.u8()?;
        v.push(CardEntry {
            id: CardId::new(set_index, coll),
            pile_slot: f & 0x07,
            known:     f & 0x08 != 0,
        });
    }
    Ok(v)
}

// ── Byte cursor ──────────────────────────────────────────────────────────────

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self { Self { data, pos: 0 } }

    fn u8(&mut self) -> Result<u8, SnapshotError> {
        if self.pos >= self.data.len() { return Err(SnapshotError::TooShort); }
        let v = self.data[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn u16(&mut self) -> Result<u16, SnapshotError> {
        if self.pos + 2 > self.data.len() { return Err(SnapshotError::TooShort); }
        let v = u16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn i16(&mut self) -> Result<i16, SnapshotError> {
        if self.pos + 2 > self.data.len() { return Err(SnapshotError::TooShort); }
        let v = i16::from_le_bytes([self.data[self.pos], self.data[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        if self.pos + n > self.data.len() { return Err(SnapshotError::TooShort); }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}

// ── v3 encoding (catalog-based, 1-byte card IDs, sparse permanent extras) ───
//
// Same header as v2.  Assumes every CardId has `set_index == 0` and
// `collector_number < 256` — i.e. the registry is acting as a catalog whose
// index fits in a u8.  The `get_registry()` in lib.rs already registers
// everything under set "DEV" with sequential u16 collector numbers, so this
// assumption is already true in practice for the web UI.
//
// Wire savings vs v2:
//   card entry:      5 bytes → 2 bytes   (no set_index, 1-byte collector)
//   permanent entry: 7 bytes → 2 bytes   (+1 per nonzero counters/loyalty byte)

pub fn encode_v3(snap: &BoardSnapshot) -> Vec<u8> {
    let mut b = Vec::with_capacity(200);
    b.push(3);
    b.push(snap.turn);
    b.push(snap.stage as u8);
    let mut flags: u8 = 0;
    if snap.on_play                 { flags |= 1; }
    if snap.us.land_drop_available  { flags |= 2; }
    if snap.opp.land_drop_available { flags |= 4; }
    b.push(flags);
    b.extend_from_slice(&snap.us.life.to_le_bytes());
    b.extend_from_slice(&snap.opp.life.to_le_bytes());
    b.extend_from_slice(&snap.life_before_dd.unwrap_or(NONE_LIFE).to_le_bytes());

    write_cards_v3(&mut b, &snap.stack);
    write_player_v3(&mut b, &snap.us);
    write_player_v3(&mut b, &snap.opp);
    b
}

pub fn decode_v3(data: &[u8]) -> Result<BoardSnapshot, SnapshotError> {
    let mut c = Cursor::new(data);
    let ver = c.u8()?;
    if ver != 3 { return Err(SnapshotError::BadVersion(ver)); }

    let turn = c.u8()?;
    let stage = match c.u8()? {
        0 => Stage::Early,
        1 => Stage::Mid,
        2 => Stage::Late,
        x => return Err(SnapshotError::BadStage(x)),
    };
    let flags = c.u8()?;
    let on_play       = flags & 1 != 0;
    let us_land_drop  = flags & 2 != 0;
    let opp_land_drop = flags & 4 != 0;

    let us_life  = c.i16()?;
    let opp_life = c.i16()?;
    let lbdd     = c.i16()?;
    let life_before_dd = if lbdd == NONE_LIFE { None } else { Some(lbdd) };

    let stack = read_cards_v3(&mut c)?;
    let mut us = read_player_v3(&mut c)?;
    us.life = us_life;
    us.land_drop_available = us_land_drop;
    let mut opp = read_player_v3(&mut c)?;
    opp.life = opp_life;
    opp.land_drop_available = opp_land_drop;

    Ok(BoardSnapshot { turn, stage, on_play, life_before_dd, stack, us, opp })
}

fn write_player_v3(b: &mut Vec<u8>, p: &PlayerSnapshot) {
    let name = p.deck_name.as_bytes();
    b.push(name.len() as u8);
    b.extend_from_slice(name);

    for zone in [&p.lands, &p.permanents] {
        b.push(zone.len() as u8);
        for e in zone {
            b.push(e.id.collector_number as u8);
            let mut f: u8 = 0;
            if e.tapped          { f |= 0x01; }
            if e.flipped         { f |= 0x02; }
            f |= (e.pile_slot & 0x07) << 2;
            let has_counters = e.counters != 0;
            let has_loyalty  = e.loyalty != 0;
            if has_counters      { f |= 0x20; }
            if has_loyalty       { f |= 0x40; }
            b.push(f);
            if has_counters { b.push(e.counters); }
            if has_loyalty  { b.push(e.loyalty);  }
        }
    }
    for zone in [&p.hand, &p.library, &p.graveyard, &p.exile] {
        write_card_zone_v3(b, zone);
    }
    b.push(p.hand_hidden);
}

/// Simple sequential encoding — used for the stack, where order matters and
/// zones are tiny (usually 0–1 entries).
fn write_cards_v3(b: &mut Vec<u8>, zone: &[CardEntry]) {
    b.push(zone.len() as u8);
    for e in zone {
        b.push(e.id.collector_number as u8);
        let mut f: u8 = e.pile_slot & 0x07;
        if e.known { f |= 0x08; }
        b.push(f);
    }
}

/// Card-zone encoding for hand/library/graveyard/exile.  Splits into:
///   [1] pile_count + pile_count × (id, flags)        — ordered, preserves slot
///   [1] distinct_count + distinct_count × (id, flags, count) — multiset rest
///
/// Wins on zones with many duplicate card_ids (realistic library, exile,
/// opp.library) and costs ~3 bytes on small unique-only zones (hand, gy).
/// The net is a big shrink on full-deck zones.
///
/// Library/exile order below the pile is NOT preserved — treat unordered zones
/// as sets.  Pile order (slots 1..=5) is preserved via the ordered section.
fn write_card_zone_v3(b: &mut Vec<u8>, zone: &[CardEntry]) {
    // Ordered section: pile cards, sorted by slot for canonical output.
    let mut pile: Vec<&CardEntry> = zone.iter().filter(|e| e.pile_slot != 0).collect();
    pile.sort_by_key(|e| e.pile_slot);
    b.push(pile.len() as u8);
    for e in pile {
        b.push(e.id.collector_number as u8);
        let mut f: u8 = e.pile_slot & 0x07;
        if e.known { f |= 0x08; }
        b.push(f);
    }

    // Multiset section: group non-pile entries by (id, known).  BTreeMap
    // gives deterministic ordering for reproducible tokens.
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(u8, bool), u32> = BTreeMap::new();
    for e in zone.iter().filter(|e| e.pile_slot == 0) {
        *groups.entry((e.id.collector_number as u8, e.known)).or_insert(0) += 1;
    }
    b.push(groups.len() as u8);
    for ((id, known), count) in &groups {
        b.push(*id);
        b.push(if *known { 0x08 } else { 0 });
        b.push(*count as u8);
    }
}

fn read_player_v3(c: &mut Cursor<'_>) -> Result<PlayerSnapshot, SnapshotError> {
    let name_len = c.u8()? as usize;
    let deck_name = String::from_utf8(c.bytes(name_len)?.to_vec())
        .map_err(|_| SnapshotError::BadUtf8)?;

    let lands      = read_permanents_v3(c)?;
    let permanents = read_permanents_v3(c)?;
    let hand       = read_card_zone_v3(c)?;
    let library    = read_card_zone_v3(c)?;
    let graveyard  = read_card_zone_v3(c)?;
    let exile      = read_card_zone_v3(c)?;
    let hand_hidden = c.u8()?;

    Ok(PlayerSnapshot {
        deck_name,
        life: 0,
        land_drop_available: false,
        lands, permanents, hand, library, graveyard, exile, hand_hidden,
    })
}

fn read_permanents_v3(c: &mut Cursor<'_>) -> Result<Vec<PermanentEntry>, SnapshotError> {
    let n = c.u8()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let coll = c.u8()? as u16;
        let f = c.u8()?;
        let counters = if f & 0x20 != 0 { c.u8()? } else { 0 };
        let loyalty  = if f & 0x40 != 0 { c.u8()? } else { 0 };
        v.push(PermanentEntry {
            id: CardId::new(0, coll),
            tapped:    f & 0x01 != 0,
            flipped:   f & 0x02 != 0,
            pile_slot: (f >> 2) & 0x07,
            counters, loyalty,
        });
    }
    Ok(v)
}

/// Inverse of `write_card_zone_v3`: read ordered pile entries, then expand
/// the multiset rest.  Pile entries come first in the output Vec (indices
/// 0..pile_count), followed by the rest.
fn read_card_zone_v3(c: &mut Cursor<'_>) -> Result<Vec<CardEntry>, SnapshotError> {
    let pile_count = c.u8()? as usize;
    let mut v = Vec::with_capacity(pile_count);
    for _ in 0..pile_count {
        let coll = c.u8()? as u16;
        let f = c.u8()?;
        v.push(CardEntry {
            id: CardId::new(0, coll),
            pile_slot: f & 0x07,
            known:     f & 0x08 != 0,
        });
    }
    let distinct = c.u8()? as usize;
    for _ in 0..distinct {
        let coll = c.u8()? as u16;
        let f = c.u8()?;
        let count = c.u8()? as usize;
        let known = f & 0x08 != 0;
        for _ in 0..count {
            v.push(CardEntry {
                id: CardId::new(0, coll),
                pile_slot: 0,
                known,
            });
        }
    }
    Ok(v)
}

fn read_cards_v3(c: &mut Cursor<'_>) -> Result<Vec<CardEntry>, SnapshotError> {
    let n = c.u8()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let coll = c.u8()? as u16;
        let f = c.u8()?;
        v.push(CardEntry {
            id: CardId::new(0, coll),
            pile_slot: f & 0x07,
            known:     f & 0x08 != 0,
        });
    }
    Ok(v)
}

// ── Base64url (RFC 4648 §5, no padding) ──────────────────────────────────────

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn to_base64url(data: &[u8]) -> String {
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64[((triple >> 18) & 0x3F) as usize] as char);
        out.push(B64[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 { out.push(B64[((triple >> 6) & 0x3F) as usize] as char); }
        if chunk.len() > 2 { out.push(B64[(triple & 0x3F) as usize] as char); }
    }
    out
}

pub fn from_base64url(s: &str) -> Result<Vec<u8>, SnapshotError> {
    let mut rev = [0xFFu8; 128];
    for (i, &ch) in B64.iter().enumerate() { rev[ch as usize] = i as u8; }

    let src = s.as_bytes();
    let mut out = Vec::with_capacity(src.len() * 3 / 4 + 1);
    let mut i = 0;

    while i < src.len() {
        let remaining = src.len() - i;
        let a = b64val(src[i], &rev)?;
        let b = b64val(src[i + 1], &rev)?;

        if remaining == 2 {
            out.push((a << 2) | (b >> 4));
            break;
        }
        let cc = b64val(src[i + 2], &rev)?;
        if remaining == 3 {
            out.push((a << 2) | (b >> 4));
            out.push((b << 4) | (cc >> 2));
            break;
        }
        let d = b64val(src[i + 3], &rev)?;
        out.push((a << 2) | (b >> 4));
        out.push((b << 4) | (cc >> 2));
        out.push((cc << 6) | d);
        i += 4;
    }
    Ok(out)
}

fn b64val(ch: u8, rev: &[u8; 128]) -> Result<u8, SnapshotError> {
    if ch >= 128 { return Err(SnapshotError::BadBase64); }
    let v = rev[ch as usize];
    if v == 0xFF { return Err(SnapshotError::BadBase64); }
    Ok(v)
}

// ── Convenience ──────────────────────────────────────────────────────────────

/// Encode a snapshot as a URL-safe base64 token.  Emits v3 (catalog-indexed,
/// 1-byte card IDs).
pub fn to_url_token(snap: &BoardSnapshot) -> String {
    to_base64url(&encode_v3(snap))
}

/// Decode a URL-safe base64 token back into a snapshot.  Dispatches on the
/// version byte so v2 tokens from before the v3 cutover still decode.
pub fn from_url_token(token: &str) -> Result<BoardSnapshot, SnapshotError> {
    let bytes = from_base64url(token)?;
    match bytes.first() {
        Some(&2) => decode(&bytes),
        Some(&3) => decode_v3(&bytes),
        Some(&v) => Err(SnapshotError::BadVersion(v)),
        None     => Err(SnapshotError::TooShort),
    }
}

// ── Errors ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum SnapshotError {
    TooShort,
    BadVersion(u8),
    BadStage(u8),
    BadUtf8,
    BadBase64,
    UnknownCard(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort            => write!(f, "unexpected end of data"),
            Self::BadVersion(v)       => write!(f, "unsupported version {v}"),
            Self::BadStage(s)         => write!(f, "invalid stage {s}"),
            Self::BadUtf8             => write!(f, "invalid UTF-8 in deck name"),
            Self::BadBase64           => write!(f, "invalid base64url character"),
            Self::UnknownCard(name)   => write!(f, "card not in registry: {name}"),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> BoardSnapshot {
        let dd_id = CardId::new(0, 42);
        let sea_id = CardId::new(1, 100);
        let ritual_id = CardId::new(2, 55);
        let oracle_id = CardId::new(0, 10);
        let petal_id = CardId::new(3, 200);
        let delta_id = CardId::new(1, 83);
        let delver_id = CardId::new(4, 11);
        let volc_id = CardId::new(1, 110);

        BoardSnapshot {
            turn: 2,
            stage: Stage::Early,
            on_play: true,
            life_before_dd: Some(20),
            stack: vec![CardEntry { id: dd_id, pile_slot: 0, known: true }],
            us: PlayerSnapshot {
                deck_name: "Doomsday".into(),
                life: 10,
                land_drop_available: false,
                lands: vec![
                    PermanentEntry {
                        id: sea_id, tapped: true, flipped: false,
                        pile_slot: 0, counters: 0, loyalty: 0,
                    },
                    PermanentEntry {
                        id: delta_id, tapped: true, flipped: false,
                        pile_slot: 0, counters: 0, loyalty: 0,
                    },
                ],
                permanents: vec![
                    PermanentEntry {
                        id: petal_id, tapped: false, flipped: false,
                        pile_slot: 0, counters: 0, loyalty: 0,
                    },
                ],
                hand: vec![],
                library: vec![
                    CardEntry { id: oracle_id, pile_slot: 1, known: true },
                    CardEntry { id: ritual_id, pile_slot: 2, known: true },
                    CardEntry { id: petal_id, pile_slot: 3, known: true },
                    CardEntry { id: sea_id, pile_slot: 4, known: true },
                    CardEntry { id: sea_id, pile_slot: 5, known: true },
                ],
                graveyard: vec![
                    CardEntry { id: ritual_id, pile_slot: 0, known: false },
                ],
                exile: vec![],
                hand_hidden: 0,
            },
            opp: PlayerSnapshot {
                deck_name: "Izzet Delver".into(),
                life: 20,
                land_drop_available: true,
                lands: vec![
                    PermanentEntry {
                        id: volc_id, tapped: false, flipped: false,
                        pile_slot: 0, counters: 0, loyalty: 0,
                    },
                ],
                permanents: vec![
                    PermanentEntry {
                        id: delver_id, tapped: false, flipped: true,
                        pile_slot: 0, counters: 0, loyalty: 0,
                    },
                ],
                hand: vec![
                    CardEntry { id: ritual_id, pile_slot: 0, known: true },
                ],
                library: vec![],
                graveyard: vec![],
                exile: vec![],
                hand_hidden: 3,
            },
        }
    }

    #[test]
    fn roundtrip_binary() {
        let snap = sample_snapshot();
        let bytes = encode(&snap);
        let decoded = decode(&bytes).unwrap();
        assert_eq!(snap, decoded);
    }

    #[test]
    fn roundtrip_base64url() {
        // v3 drops `set_index` (the real registry always uses set_index=0),
        // and zone order below the pile isn't preserved, so flatten and
        // canonicalize before roundtripping.
        let mut snap = sample_snapshot();
        let fix = |c: &mut CardId| *c = CardId::new(0, c.collector_number);
        for e in &mut snap.stack { fix(&mut e.id); }
        for p in [&mut snap.us, &mut snap.opp] {
            for e in &mut p.lands      { fix(&mut e.id); }
            for e in &mut p.permanents { fix(&mut e.id); }
            for e in &mut p.hand       { fix(&mut e.id); }
            for e in &mut p.library    { fix(&mut e.id); }
            for e in &mut p.graveyard  { fix(&mut e.id); }
            for e in &mut p.exile      { fix(&mut e.id); }
        }
        canonicalize(&mut snap);
        let token = to_url_token(&snap);
        let decoded = from_url_token(&token).unwrap();
        assert_eq!(snap, decoded);

        // Verify the token is URL-safe (no +, /, or =).
        assert!(!token.contains('+'));
        assert!(!token.contains('/'));
        assert!(!token.contains('='));
    }

    #[test]
    fn snapshot_size() {
        let snap = sample_snapshot();
        let bytes = encode(&snap);
        let token = to_url_token(&snap);
        // Rough sanity: binary should be under 300 bytes, token under 400 chars.
        assert!(bytes.len() < 300, "binary too large: {} bytes", bytes.len());
        assert!(token.len() < 400, "token too large: {} chars", token.len());
        eprintln!("binary: {} bytes, base64url: {} chars", bytes.len(), token.len());
    }

    #[test]
    fn base64url_roundtrip_various_lengths() {
        for len in 0..20 {
            let data: Vec<u8> = (0..len).collect();
            let encoded = to_base64url(&data);
            let decoded = from_base64url(&encoded).unwrap();
            assert_eq!(data, decoded, "failed at len={len}");
        }
    }

    #[test]
    fn pile_slot_survives_roundtrip() {
        let snap = sample_snapshot();
        let bytes = encode(&snap);
        let decoded = decode(&bytes).unwrap();
        // Library cards were assigned slots 1..=5 in sample_snapshot.
        let slots: Vec<u8> = decoded.us.library.iter().map(|c| c.pile_slot).collect();
        assert_eq!(slots, vec![1, 2, 3, 4, 5]);
        // Graveyard card is not in the pile.
        assert!(decoded.us.graveyard.iter().all(|c| c.pile_slot == 0));
    }

    #[test]
    fn decode_bad_version() {
        let snap = sample_snapshot();
        let mut bytes = encode(&snap);
        bytes[0] = 99;
        assert!(matches!(decode(&bytes), Err(SnapshotError::BadVersion(99))));
    }

    #[test]
    fn decode_truncated() {
        assert!(matches!(decode(&[VERSION, 2]), Err(SnapshotError::TooShort)));
    }

    fn sample_registry() -> CardRegistry {
        CardRegistry::from_entries(&[
            ("Doomsday",            "WTH", 42),
            ("Underground Sea",     "3ED", 100),
            ("Dark Ritual",         "LEA", 55),
            ("Thassa's Oracle",     "THB", 10),
            ("Lotus Petal",         "TMP", 200),
            ("Polluted Delta",      "ONS", 83),
            ("Delver of Secrets",   "ISD", 11),
            ("Volcanic Island",     "3ED", 110),
        ])
    }

    #[test]
    fn registry_roundtrip() {
        let reg = sample_registry();
        let id = reg.name_to_id("Dark Ritual").unwrap();
        let name = reg.id_to_name(id).unwrap();
        assert_eq!(name, "Dark Ritual");
    }

    /// Canonicalize a snapshot for comparison after a multiset roundtrip:
    /// card zones' non-pile entries are sorted by (id, known) — the same
    /// ordering the BTreeMap in `write_card_zone_v3` produces.
    fn canonicalize(s: &mut BoardSnapshot) {
        fn sort_zone(z: &mut Vec<CardEntry>) {
            let (pile, mut rest): (Vec<_>, Vec<_>) =
                z.drain(..).partition(|e| e.pile_slot != 0);
            let mut pile = pile;
            pile.sort_by_key(|e| e.pile_slot);
            rest.sort_by_key(|e| (e.id.collector_number, e.known));
            z.extend(pile);
            z.extend(rest);
        }
        for p in [&mut s.us, &mut s.opp] {
            sort_zone(&mut p.hand);
            sort_zone(&mut p.library);
            sort_zone(&mut p.graveyard);
            sort_zone(&mut p.exile);
        }
    }

    #[test]
    fn v3_roundtrip() {
        let mut snap = sample_snapshot();
        // sample_snapshot uses nonzero set_index; rewrite ids as if all from one set.
        let fix = |c: &mut CardId| *c = CardId::new(0, c.collector_number);
        for e in &mut snap.stack { fix(&mut e.id); }
        for p in [&mut snap.us, &mut snap.opp] {
            for e in &mut p.lands      { fix(&mut e.id); }
            for e in &mut p.permanents { fix(&mut e.id); }
            for e in &mut p.hand       { fix(&mut e.id); }
            for e in &mut p.library    { fix(&mut e.id); }
            for e in &mut p.graveyard  { fix(&mut e.id); }
            for e in &mut p.exile      { fix(&mut e.id); }
        }
        canonicalize(&mut snap);
        let bytes = encode_v3(&snap);
        let decoded = decode_v3(&bytes).unwrap();
        assert_eq!(snap, decoded);
    }

    /// Realistic post-DD snapshot: the 5-card pile is now the library, the
    /// remaining ~48 deck cards are in exile (mirroring DD's "exile the rest"
    /// effect), opponent has a plausible board and a full 53-card library.
    ///
    /// Decks are encoded as (id, count) pairs so duplicate cards actually
    /// deduplicate under multiset encoding — matching real MTG decks
    /// (4x staples, basics, etc.).
    fn realistic_snapshot() -> BoardSnapshot {
        let c = |id: u16| CardId::new(0, id);
        let perm = |id: u16, tapped: bool| PermanentEntry {
            id: c(id), tapped, flipped: false, pile_slot: 0,
            counters: 0, loyalty: 0,
        };
        fn expand(items: &[(u16, usize)], known: bool) -> Vec<CardEntry> {
            let mut v = Vec::new();
            for &(id, n) in items {
                for _ in 0..n {
                    v.push(CardEntry {
                        id: CardId::new(0, id),
                        pile_slot: 0, known,
                    });
                }
            }
            v
        }

        // DD decklist (28 distinct cards, 60 total) — mirrors dd_deck() in lib.rs.
        let dd_deck: &[(u16, usize)] = &[
            (0, 3), (1, 4), (2, 1), (3, 1), (4, 1), (5, 1), (6, 1), (7, 1),
            (8, 2), (9, 3), (10, 1), (11, 2), (12, 1),
            (13, 4), (14, 4), (15, 4), (16, 4), (17, 1), (18, 1),
            (19, 4), (20, 3), (21, 2), (22, 1), (23, 1), (24, 1),
            (25, 4), (26, 2), (27, 2),
        ];
        // Post-DD: library is just the 5 pile cards (slots 1..5).
        let pile_cards: Vec<CardEntry> = (0..5)
            .map(|slot| CardEntry {
                id: c(100 + slot as u16), pile_slot: slot + 1, known: true,
            })
            .collect();
        // Exile: everything from the deck except what's elsewhere.  For this
        // test, pretend everything except a small board+hand+gy went to exile.
        let us_exile = expand(dd_deck, true);  // ~60 cards with heavy dupes

        let us = PlayerSnapshot {
            deck_name: "Doomsday".into(),
            life: 10,
            land_drop_available: false,
            lands: vec![perm(1, true), perm(1, true), perm(8, false)],
            permanents: vec![],
            hand: vec![
                CardEntry { id: c(13), pile_slot: 0, known: true },
                CardEntry { id: c(19), pile_slot: 0, known: true },
            ],
            library: pile_cards,
            graveyard: vec![
                CardEntry { id: c(14), pile_slot: 0, known: true },
            ],
            exile: us_exile,
            hand_hidden: 0,
        };

        // Izzet Delver decklist (~21 distinct, 60 total) — from lib.rs.
        let delver_deck: &[(u16, usize)] = &[
            (50, 4), (51, 2), (52, 2), (53, 2), (54, 3), (55, 4),
            (56, 1), (57, 1),
            (58, 3), (59, 4), (60, 2), (61, 1),
            (62, 3), (63, 4),
            (64, 4), (65, 1),
            (66, 4), (67, 1), (68, 4),
            (69, 4), (70, 4), (71, 2),
        ];
        let opp_library = expand(delver_deck, false);

        let opp = PlayerSnapshot {
            deck_name: "Izzet Delver".into(),
            life: 20,
            land_drop_available: true,
            lands: vec![perm(50, false), perm(51, true)],
            permanents: vec![
                PermanentEntry { id: c(58), tapped: false, flipped: true,
                    pile_slot: 0, counters: 0, loyalty: 0 },
            ],
            hand: vec![],
            library: opp_library,
            graveyard: vec![
                CardEntry { id: c(64), pile_slot: 0, known: false },
            ],
            exile: vec![],
            hand_hidden: 3,
        };

        BoardSnapshot {
            turn: 3, stage: Stage::Early, on_play: true,
            life_before_dd: Some(20),
            stack: vec![CardEntry { id: c(15), pile_slot: 0, known: true }],
            us, opp,
        }
    }

    /// Redact hidden opponent zones — drop library entirely and keep only
    /// revealed cards in hand.  Library count is lost (this is "public view").
    fn redact_opp(snap: &BoardSnapshot) -> BoardSnapshot {
        let mut s = snap.clone();
        s.opp.library.clear();
        s.opp.hand = s.opp.hand.iter().filter(|c| c.known).cloned().collect();
        s
    }

    #[test]
    fn from_url_token_decodes_both_v2_and_v3() {
        // v2: encode a snapshot with the v2 encoder, then decode via the
        // version-dispatching entry point.
        let snap2 = sample_snapshot();
        let v2_token = to_base64url(&encode(&snap2));
        assert_eq!(from_url_token(&v2_token).unwrap(), snap2);

        // v3: sample_snapshot uses nonzero set_index, so flatten first.
        let mut snap3 = sample_snapshot();
        let fix = |c: &mut CardId| *c = CardId::new(0, c.collector_number);
        for e in &mut snap3.stack { fix(&mut e.id); }
        for p in [&mut snap3.us, &mut snap3.opp] {
            for e in &mut p.lands      { fix(&mut e.id); }
            for e in &mut p.permanents { fix(&mut e.id); }
            for e in &mut p.hand       { fix(&mut e.id); }
            for e in &mut p.library    { fix(&mut e.id); }
            for e in &mut p.graveyard  { fix(&mut e.id); }
            for e in &mut p.exile      { fix(&mut e.id); }
        }
        let v3_token = to_url_token(&snap3);
        assert_eq!(from_url_token(&v3_token).unwrap(), snap3);
    }

    #[test]
    fn compare_v2_v3_sizes() {
        let snap = realistic_snapshot();

        let v2_bytes = encode(&snap);
        let v2_token = to_base64url(&v2_bytes);

        let v3_bytes = encode_v3(&snap);
        let v3_token = to_base64url(&v3_bytes);

        let redacted = redact_opp(&snap);
        let v3r_bytes = encode_v3(&redacted);
        let v3r_token = to_base64url(&v3r_bytes);

        let base_url = "https://bur.io/doomsday/pilegen/#s=";

        eprintln!("\n── URL size comparison ──────────────────────────────");
        eprintln!("v2 (current):              {:>4} bytes / {:>4} chars base64",
            v2_bytes.len(), v2_token.len());
        eprintln!("v3 (catalog 1-byte IDs):   {:>4} bytes / {:>4} chars base64",
            v3_bytes.len(), v3_token.len());
        eprintln!("v3 + opp redacted:         {:>4} bytes / {:>4} chars base64",
            v3r_bytes.len(), v3r_token.len());
        eprintln!();
        eprintln!("Full URLs (with {} prefix):", base_url);
        eprintln!("  v2:       {}{}", base_url, v2_token);
        eprintln!();
        eprintln!("  v3:       {}{}", base_url, v3_token);
        eprintln!();
        eprintln!("  v3 red.:  {}{}", base_url, v3r_token);
        eprintln!();

        // Sanity: v3 must roundtrip.
        assert_eq!(snap, decode_v3(&v3_bytes).unwrap());
    }
}
