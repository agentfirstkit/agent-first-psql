//! What `ui schema`, `ui table` and `ui indexes` put in front of a person.
//!
//! These three panels run the schema-snapshot query, which is a *wire* shape:
//! one row per object, eight kinds of object unioned into one set of generic
//! columns, most of them null for any given row, and the facts a person came
//! for — nullable, primary key, unique, owner, comment — packed into a `payload`
//! object. That is the right shape for an agent, which reads `kind` and picks
//! the fields it wants. It is the wrong shape for a person, who reads a table
//! the way `\d` presents one: an object, then its columns, then the things
//! attached to it.
//!
//! So this module is the translation, and it happens here rather than in a
//! template on purpose. Grouping rows by relation, dropping the constraint rows
//! that only restate a column's own `not null`, turning `persistence: "p"` into
//! `permanent` and `estimated_rows: -1` into "never analyzed" are all
//! judgements about what is true, and [`super`]'s contract puts those on
//! afpsql's side of the line. A template decides whether a column list is a
//! `<table>` or a `<dl>`; it never has to know that `-1` is not a row count.
//!
//! The unaltered result set stays on the page as [`super::TableDocument`], so
//! nothing this module chooses to summarize is thereby hidden.

use std::collections::HashMap;
use std::fmt::Write as _;

use serde::Serialize;
use serde_json::Value;

use super::{TableDocument, table_document};
use crate::cli::UiView;
use crate::types::ColumnInfo;

/// The panel document for whichever inspection was asked for.
///
/// Untagged, so a template sees the panel's own fields at `document.…` rather
/// than one level down inside a variant name that means nothing to it.
#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum InspectDocument {
    Snapshot(Box<SnapshotDocument>),
    Indexes(Box<IndexPanelDocument>),
}

pub(super) fn inspect_document(
    view: &UiView,
    subject: &str,
    columns: &[ColumnInfo],
    rows: &[Value],
) -> InspectDocument {
    match view {
        UiView::Indexes { .. } => {
            InspectDocument::Indexes(Box::new(index_panel_document(view, subject, columns, rows)))
        }
        _ => InspectDocument::Snapshot(Box::new(snapshot_document(view, subject, columns, rows))),
    }
}

// ═══════════════════════════════════════════
// The pieces a relation is read in
// ═══════════════════════════════════════════

/// One line on an object's card: a fact the server reported, already worded.
///
/// A list rather than a struct of optional fields because the card's whole job
/// is to say what is known — a fact the server did not report is absent from
/// this list, and a template renders what it is given without a condition per
/// fact.
#[derive(Serialize)]
pub(super) struct FactDocument {
    label: &'static str,
    value: String,
}

/// One column of a relation, with everything `payload` held already unpacked.
#[derive(Serialize)]
pub(super) struct ColumnEntry {
    position: i64,
    name: String,
    type_name: String,
    /// PostgreSQL's answer, not an absence: a template that renders nothing for
    /// a nullable column is choosing to, rather than being unable to tell.
    nullable: bool,
    /// What the column gets when a statement does not name it. An identity or a
    /// generated column carries its expression here too, with the tag beside it
    /// saying which of the three this is.
    default: Option<String>,
    tags: Vec<String>,
    comment: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ConstraintEntry {
    name: String,
    /// `foreign key`, `check`, `unique`, `exclusion` — PostgreSQL's own word.
    kind: String,
    definition: Option<String>,
    tags: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct IndexEntry {
    name: String,
    /// The access method: `btree`, `gin`, `brin`.
    method: String,
    /// The whole `CREATE INDEX`, which is the only place the indexed
    /// expressions appear.
    definition: Option<String>,
    size: Option<String>,
    tags: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct TriggerEntry {
    name: String,
    definition: Option<String>,
    function: Option<String>,
    tags: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct ExtensionEntry {
    name: String,
    version: Option<String>,
}

#[derive(Serialize)]
pub(super) struct RoutineEntry {
    /// Already `name(argument types)`, as PostgreSQL identifies it — two
    /// overloads of one name are two entries that can be told apart.
    name: String,
    language: Option<String>,
    returns: Option<String>,
    definition: Option<String>,
}

/// One relation and everything attached to it.
#[derive(Serialize)]
pub(super) struct RelationDocument {
    /// `schema.name`, which is also what tells two relations apart in a list.
    key: String,
    schema: String,
    name: String,
    /// `table`, `view`, `materialized view`, `sequence`, …
    object_type: String,
    /// What this relation amounts to in one line — `2 columns · 1 index · 16
    /// kB` — so a collapsed entry still says whether it is worth opening.
    summary: String,
    facts: Vec<FactDocument>,
    comment: Option<String>,
    /// A view's body. Only a view or a materialized view has one.
    definition: Option<String>,
    columns: Vec<ColumnEntry>,
    constraints: Vec<ConstraintEntry>,
    indexes: Vec<IndexEntry>,
    triggers: Vec<TriggerEntry>,
}

/// `schema_inspect` and `table_inspect`.
///
/// One document for both, because they are one query narrowed two ways: a table
/// panel is a schema panel holding a single relation. What differs is how much
/// a person wants opened at once, which is [`Self::expanded`].
#[derive(Serialize)]
pub(super) struct SnapshotDocument {
    ui_kind: &'static str,
    title: String,
    heading: String,
    subject: String,
    /// Always `null`: a schema does not change while a person reads it. The key
    /// is present so one layout serves every panel.
    refresh_seconds: Option<u64>,
    footer: String,
    row_count: usize,
    /// Whether each relation's detail is worth opening unasked. A table panel
    /// always is; a schema of forty relations is a wall.
    expanded: bool,
    /// afpsql's own sentence for a panel with nothing on it, or `null`. Said
    /// here rather than in a template because "no such table" and "an empty
    /// schema" are different answers.
    empty: Option<String>,
    relations: Vec<RelationDocument>,
    extensions: Vec<ExtensionEntry>,
    routines: Vec<RoutineEntry>,
    /// The result set exactly as the server returned it, unsummarized.
    table: TableDocument,
}

/// One table's indexes, in `index_inspect`.
#[derive(Serialize)]
pub(super) struct IndexGroupDocument {
    table: String,
    indexes: Vec<IndexEntry>,
}

/// `index_inspect`.
///
/// Its rows are already one shape — every row is an index — so the work here is
/// grouping them under the table they belong to and unpacking the four booleans
/// that decide whether an index is doing its job.
#[derive(Serialize)]
pub(super) struct IndexPanelDocument {
    ui_kind: &'static str,
    title: String,
    heading: String,
    subject: String,
    refresh_seconds: Option<u64>,
    footer: String,
    row_count: usize,
    empty: Option<String>,
    groups: Vec<IndexGroupDocument>,
    table: TableDocument,
}

// ═══════════════════════════════════════════
// Building a snapshot panel
// ═══════════════════════════════════════════

/// Relations are collected first and attached to second, rather than in one
/// pass, because a child row is only meaningful once its parent exists. The
/// query already returns them in that order; doing it in two passes means the
/// panel does not depend on that and cannot silently drop a column whose
/// relation happened to sort after it.
fn snapshot_document(
    view: &UiView,
    subject: &str,
    columns: &[ColumnInfo],
    rows: &[Value],
) -> SnapshotDocument {
    let mut relations: Vec<RelationDocument> = Vec::new();
    let mut slots: HashMap<String, usize> = HashMap::new();
    let mut extensions: Vec<ExtensionEntry> = Vec::new();
    let mut routines: Vec<RoutineEntry> = Vec::new();

    for row in rows {
        match super::text(row, "kind").as_str() {
            "extension" => extensions.push(ExtensionEntry {
                name: super::text(row, "name"),
                version: fact(row, "version"),
            }),
            "function" => routines.push(RoutineEntry {
                name: super::text(row, "name"),
                language: fact(row, "language"),
                returns: fact(row, "result"),
                definition: optional(row, "definition"),
            }),
            "relation" | "sequence" => {
                let relation = relation_document(row);
                slots.entry(relation.key.clone()).or_insert(relations.len());
                relations.push(relation);
            }
            _ => {}
        }
    }

    for row in rows {
        let kind = super::text(row, "kind");
        if !matches!(kind.as_str(), "column" | "constraint" | "index" | "trigger") {
            continue;
        }
        let slot = slot_for(row, &mut relations, &mut slots);
        let relation = &mut relations[slot];
        match kind.as_str() {
            "column" => relation.columns.push(column_entry(row)),
            // A `NOT NULL` constraint is the column's own `nullable` said a
            // second time, under a generated name a person never wrote. Every
            // column already carries the answer, so listing these would put
            // one redundant row per non-null column between a reader and the
            // constraints they came to check.
            "constraint" if fact(row, "type").as_deref() == Some("n") => {}
            "constraint" => relation.constraints.push(constraint_entry(row)),
            "index" => relation.indexes.push(index_entry(row)),
            _ => relation.triggers.push(trigger_entry(row)),
        }
    }

    for relation in &mut relations {
        // The kind leads the collapsed line, because in a list of forty that is
        // the first thing being scanned for.
        relation.summary = std::iter::once(relation.object_type.clone())
            .chain(relation_parts(relation))
            .collect::<Vec<_>>()
            .join(" · ");
    }

    let table_panel = matches!(view, UiView::Table { .. });
    // The tab and the heading are the same word, so a panel opened on a view
    // does not sit in a window labelled "Table".
    let heading = snapshot_heading(view, &relations);
    SnapshotDocument {
        ui_kind: view.ui_kind(),
        title: heading.clone(),
        heading,
        subject: subject.to_owned(),
        refresh_seconds: None,
        footer: snapshot_footer(table_panel, &relations, &extensions, &routines),
        row_count: rows.len(),
        expanded: table_panel || relations.len() <= 3,
        empty: snapshot_empty(table_panel, subject, &relations, &extensions, &routines),
        relations,
        extensions,
        routines,
        table: table_document(columns, rows),
    }
}

/// Where this row's relation lives, adding a bare one if the snapshot never
/// described it.
///
/// The query cannot produce such a row today — every child joins
/// `relation_filter`. It is handled anyway because the alternative is dropping
/// data on the floor: a person reading a panel should never be short a column
/// because afpsql could not find its heading.
fn slot_for(
    row: &Value,
    relations: &mut Vec<RelationDocument>,
    slots: &mut HashMap<String, usize>,
) -> usize {
    let schema = super::text(row, "schema");
    let name = super::text(row, "relation");
    let key = format!("{schema}.{name}");
    if let Some(slot) = slots.get(&key) {
        return *slot;
    }
    let slot = relations.len();
    relations.push(RelationDocument {
        key: key.clone(),
        schema,
        name,
        object_type: "relation".to_owned(),
        summary: String::new(),
        facts: Vec::new(),
        comment: None,
        definition: None,
        columns: Vec::new(),
        constraints: Vec::new(),
        indexes: Vec::new(),
        triggers: Vec::new(),
    });
    slots.insert(key, slot);
    slot
}

fn relation_document(row: &Value) -> RelationDocument {
    let schema = super::text(row, "schema");
    let name = super::text(row, "name");
    let object_type = super::text(row, "object_type");
    let mut facts = Vec::new();
    if let Some(owner) = fact(row, "owner") {
        facts.push(FactDocument {
            label: "owner",
            value: owner,
        });
    }
    // A view has no storage, so a size of "0 bytes" beside it would be a fact
    // about nothing. The query leaves it null for exactly those relations.
    if let Some(size) = optional(row, "size") {
        facts.push(FactDocument {
            label: "size",
            value: size,
        });
    }
    if let Some(rows_estimate) = estimated_rows(row, &object_type) {
        facts.push(FactDocument {
            label: "rows",
            value: rows_estimate,
        });
    }
    // Only worth a line when it is not the default. Every ordinary table is
    // permanent, and saying so on every card teaches a reader to skip the card.
    if let Some(persistence) = fact(row, "persistence").and_then(|code| match code.as_str() {
        "u" => Some("unlogged — not crash-safe, not replicated"),
        "t" => Some("temporary — this session only"),
        _ => None,
    }) {
        facts.push(FactDocument {
            label: "storage",
            value: persistence.to_owned(),
        });
    }
    RelationDocument {
        key: format!("{schema}.{name}"),
        schema,
        name,
        object_type,
        summary: String::new(),
        facts,
        comment: fact(row, "comment"),
        definition: optional(row, "definition"),
        columns: Vec::new(),
        constraints: Vec::new(),
        indexes: Vec::new(),
        triggers: Vec::new(),
    }
}

/// What `reltuples` means, rather than what it says.
///
/// `-1` is PostgreSQL for "this relation has never been analyzed", which reads
/// as a row count of minus one to everyone who has not memorized that. It is
/// also only an estimate when it is a number at all, so it never appears
/// unqualified.
fn estimated_rows(row: &Value, object_type: &str) -> Option<String> {
    if !matches!(
        object_type,
        "table" | "partitioned table" | "materialized view"
    ) {
        return None;
    }
    match super::number(row, "estimated_rows") {
        None => None,
        Some(count) if count < 0 => Some("unknown — never analyzed".to_owned()),
        Some(count) => Some(format!("≈ {}", grouped(count))),
    }
}

fn column_entry(row: &Value) -> ColumnEntry {
    let mut tags = Vec::new();
    if flag(row, "primary_key") {
        tags.push("primary key".to_owned());
    }
    match fact(row, "identity").as_deref() {
        Some("a") => tags.push("identity, always".to_owned()),
        Some("d") => tags.push("identity, by default".to_owned()),
        _ => {}
    }
    if fact(row, "generated").as_deref() == Some("s") {
        tags.push("generated, stored".to_owned());
    }
    ColumnEntry {
        position: super::number(row, "position").unwrap_or_default(),
        name: super::text(row, "name"),
        // The snapshot puts a column's data type in `object_type`, which is
        // where an index keeps its access method and a constraint its kind.
        // One generic column, three meanings — so it is named here.
        type_name: super::text(row, "object_type"),
        nullable: flag(row, "nullable"),
        default: optional(row, "definition"),
        tags,
        comment: fact(row, "comment"),
    }
}

fn constraint_entry(row: &Value) -> ConstraintEntry {
    let mut tags = Vec::new();
    if flag(row, "deferrable") {
        tags.push(if flag(row, "deferred_by_default") {
            "deferrable, initially deferred".to_owned()
        } else {
            "deferrable".to_owned()
        });
    }
    // `NOT VALID` means PostgreSQL enforces this constraint on new rows and has
    // never checked the ones already there. A reader who takes it as a
    // guarantee about the table would be wrong, so it is said out loud.
    if !flag(row, "validated") {
        tags.push("not validated against existing rows".to_owned());
    }
    ConstraintEntry {
        name: super::text(row, "name"),
        kind: super::text(row, "object_type"),
        definition: optional(row, "definition"),
        tags,
    }
}

fn index_entry(row: &Value) -> IndexEntry {
    let mut tags = Vec::new();
    if flag(row, "primary") {
        tags.push("primary key".to_owned());
    } else if flag(row, "unique") {
        tags.push("unique".to_owned());
    }
    // The two states a person needs to notice: an index the planner will not
    // use, and one a concurrent build never finished.
    if !flag(row, "valid") {
        tags.push("invalid — the planner ignores it".to_owned());
    }
    if !flag(row, "ready") {
        tags.push("not ready — still being built".to_owned());
    }
    IndexEntry {
        name: super::text(row, "name"),
        method: super::text(row, "object_type"),
        definition: optional(row, "definition"),
        size: optional(row, "size"),
        tags,
    }
}

fn trigger_entry(row: &Value) -> TriggerEntry {
    let mut tags = Vec::new();
    match fact(row, "enabled").as_deref() {
        Some("D") => tags.push("disabled".to_owned()),
        Some("R") => tags.push("replica only".to_owned()),
        Some("A") => tags.push("always, replica included".to_owned()),
        _ => {}
    }
    let function = match (fact(row, "function_schema"), fact(row, "function_name")) {
        (Some(schema), Some(name)) => Some(format!("{schema}.{name}()")),
        (None, Some(name)) => Some(format!("{name}()")),
        _ => None,
    };
    TriggerEntry {
        name: super::text(row, "name"),
        definition: optional(row, "definition"),
        function,
        tags,
    }
}

/// What one relation holds and what it costs, each part only when there is one.
///
/// Without the kind, which two callers want in two different places: leading a
/// collapsed line in a schema panel, and left out of a table panel's footer
/// where the heading above it already says what the object is.
fn relation_parts(relation: &RelationDocument) -> Vec<String> {
    let mut parts = Vec::new();
    for (count, singular) in [
        (relation.columns.len(), "column"),
        (relation.constraints.len(), "constraint"),
        (relation.indexes.len(), "index"),
        (relation.triggers.len(), "trigger"),
    ] {
        if count > 0 {
            parts.push(plural(count, singular));
        }
    }
    if let Some(size) = relation
        .facts
        .iter()
        .find(|fact| fact.label == "size")
        .map(|fact| fact.value.clone())
    {
        parts.push(size);
    }
    parts
}

/// The window's own heading.
///
/// A table panel is named for what it opened onto, so pointing it at a view
/// says "View" rather than insisting the object is a table. A schema panel
/// keeps its own word: it is a view of many things at once.
fn snapshot_heading(view: &UiView, relations: &[RelationDocument]) -> String {
    match (view, relations) {
        (UiView::Table { .. }, [relation]) => {
            let mut heading = relation.object_type.clone();
            if let Some(first) = heading.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            heading
        }
        _ => view.title().to_owned(),
    }
}

fn snapshot_footer(
    table_panel: bool,
    relations: &[RelationDocument],
    extensions: &[ExtensionEntry],
    routines: &[RoutineEntry],
) -> String {
    let mut footer = String::new();
    if table_panel {
        // One relation: what a reader counts is what is inside it, not that
        // there is one of it — and not what kind it is, which the heading two
        // inches up already said.
        if let Some(relation) = relations.first() {
            footer.push_str(&relation_parts(relation).join(" · "));
        }
    } else {
        let mut parts: Vec<String> = Vec::new();
        // Relations counted by what they are, because "9 objects" in a schema
        // holding 3 tables and 6 sequences answers no question anyone has.
        let mut kinds: Vec<(String, usize)> = Vec::new();
        for relation in relations {
            match kinds
                .iter_mut()
                .find(|(name, _)| *name == relation.object_type)
            {
                Some((_, count)) => *count += 1,
                None => kinds.push((relation.object_type.clone(), 1)),
            }
        }
        for (kind, count) in kinds {
            parts.push(plural(count, &kind));
        }
        if !extensions.is_empty() {
            parts.push(plural(extensions.len(), "extension"));
        }
        if !routines.is_empty() {
            parts.push(plural(routines.len(), "function"));
        }
        footer.push_str(&parts.join(" · "));
    }
    if !footer.is_empty() {
        footer.push('.');
    }
    footer
}

/// The sentence a panel with nothing on it says.
///
/// A table panel and a schema panel are empty for different reasons, and
/// "No matching objects." would send a reader looking for the wrong mistake.
fn snapshot_empty(
    table_panel: bool,
    subject: &str,
    relations: &[RelationDocument],
    extensions: &[ExtensionEntry],
    routines: &[RoutineEntry],
) -> Option<String> {
    if !relations.is_empty() || !extensions.is_empty() || !routines.is_empty() {
        return None;
    }
    Some(if table_panel {
        format!("No table, view or sequence named {subject} exists on this server.")
    } else {
        format!("The schema {subject} holds no objects.")
    })
}

// ═══════════════════════════════════════════
// Building an index panel
// ═══════════════════════════════════════════

fn index_panel_document(
    view: &UiView,
    subject: &str,
    columns: &[ColumnInfo],
    rows: &[Value],
) -> IndexPanelDocument {
    let mut groups: Vec<IndexGroupDocument> = Vec::new();
    for row in rows {
        let table = format!(
            "{}.{}",
            super::text(row, "schema"),
            super::text(row, "table")
        );
        let mut tags = Vec::new();
        if super::boolean(row, "primary") {
            tags.push("primary key".to_owned());
        } else if super::boolean(row, "unique") {
            tags.push("unique".to_owned());
        }
        if !super::boolean(row, "valid") {
            tags.push("invalid — the planner ignores it".to_owned());
        }
        if !super::boolean(row, "ready") {
            tags.push("not ready — still being built".to_owned());
        }
        let index = IndexEntry {
            name: super::text(row, "name"),
            method: super::text(row, "method"),
            definition: optional(row, "definition"),
            size: optional(row, "size"),
            tags,
        };
        match groups.iter_mut().find(|group| group.table == table) {
            Some(group) => group.indexes.push(index),
            None => groups.push(IndexGroupDocument {
                table,
                indexes: vec![index],
            }),
        }
    }

    let mut footer = String::new();
    if !groups.is_empty() {
        let _ = write!(
            footer,
            "{} on {}.",
            plural(rows.len(), "index"),
            plural(groups.len(), "table")
        );
    }

    IndexPanelDocument {
        ui_kind: view.ui_kind(),
        title: view.title().to_owned(),
        heading: view.title().to_owned(),
        subject: subject.to_owned(),
        refresh_seconds: None,
        footer,
        row_count: rows.len(),
        empty: groups
            .is_empty()
            .then(|| format!("Nothing in {subject} is indexed.")),
        groups,
        table: table_document(columns, rows),
    }
}

// ═══════════════════════════════════════════
// Reading one snapshot row
// ═══════════════════════════════════════════

/// A top-level column, or `None` when the server had nothing to put there.
///
/// The snapshot's columns are generic, so most are null on any given row —
/// `size` on a view, `definition` on a table. `None` and `Some("")` are the
/// same answer here on purpose: neither is something to render.
fn optional(row: &Value, key: &str) -> Option<String> {
    row.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// A string out of the row's `payload`, which is where the snapshot keeps
/// everything that does not fit its generic columns.
///
/// `''` is filtered out because that is how PostgreSQL spells "not an identity
/// column" and "not generated" — an empty answer, not an empty value.
fn fact(row: &Value, key: &str) -> Option<String> {
    row.get("payload")
        .and_then(|payload| payload.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// A boolean out of `payload`. Absent reads as false, which is what every
/// caller here wants: an index with no `unique` key is not a unique index.
fn flag(row: &Value, key: &str) -> bool {
    row.get("payload")
        .and_then(|payload| payload.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// `1 column`, `2 columns` — the count and its noun, agreeing.
fn plural(count: usize, singular: &str) -> String {
    if count == 1 {
        return format!("1 {singular}");
    }
    // `index` is the one noun here that does not take a plain `s`. The rest,
    // including the two-word ones the snapshot produces (`partitioned table`,
    // `materialized view`), pluralize on their last word.
    let plural = match singular {
        "index" => "indexes".to_owned(),
        other => format!("{other}s"),
    };
    format!("{count} {plural}")
}

/// A count a person can read at a glance rather than count digits in.
fn grouped(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if value < 0 {
        return format!("-{grouped}");
    }
    grouped
}
