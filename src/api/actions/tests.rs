use super::*;

fn id(n: u8) -> Uuid {
    Uuid::from_bytes([n; 16])
}

fn item(targets: Vec<Target>) -> NewItem {
    NewItem {
        kind: "approval.charge".into(),
        event_id: None,
        inhibitor_id: None,
        payload: serde_json::json!({}),
        targets,
        expires_at: None,
    }
}

// --- validate -------------------------------------------------------------

#[test]
fn an_item_with_a_kind_and_a_target_is_valid() {
    assert!(validate(&item(vec![Target::Role(id(1))])).is_ok());
}

#[test]
fn an_item_with_no_targets_is_refused() {
    // The leak this guards: it would insert cleanly, count towards nobody's
    // badge, and never be settled.
    let err = validate(&item(vec![])).unwrap_err();
    assert!(matches!(err, ActionError::Invalid(_)));
}

#[test]
fn an_item_with_an_empty_kind_is_refused() {
    let mut i = item(vec![Target::User(id(1))]);
    i.kind = String::new();
    assert!(matches!(validate(&i).unwrap_err(), ActionError::Invalid(_)));
}

#[test]
fn a_kind_of_only_whitespace_is_refused() {
    let mut i = item(vec![Target::User(id(1))]);
    i.kind = "   ".into();
    assert!(matches!(validate(&i).unwrap_err(), ActionError::Invalid(_)));
}

#[test]
fn a_payload_that_is_not_an_object_is_refused() {
    // The column is jsonb, so a bare array or string stores fine and breaks
    // whoever reads a field out of it.
    let mut i = item(vec![Target::User(id(1))]);
    i.payload = serde_json::json!([1, 2, 3]);
    assert!(matches!(validate(&i).unwrap_err(), ActionError::Invalid(_)));
}

// --- dedupe_targets -------------------------------------------------------

#[test]
fn exact_repeats_collapse() {
    let targets = vec![Target::Role(id(1)), Target::Role(id(1))];
    assert_eq!(dedupe_targets(&targets), vec![Target::Role(id(1))]);
}

#[test]
fn a_role_and_a_user_with_the_same_uuid_are_different_targets() {
    // Nothing stops a role id and a user id colliding, and collapsing them
    // would drop a target silently.
    let targets = vec![Target::Role(id(1)), Target::User(id(1))];
    assert_eq!(dedupe_targets(&targets).len(), 2);
}

#[test]
fn dedupe_keeps_the_order_given() {
    let targets = vec![
        Target::User(id(3)),
        Target::Role(id(1)),
        Target::User(id(3)),
        Target::Role(id(2)),
    ];
    assert_eq!(
        dedupe_targets(&targets),
        vec![
            Target::User(id(3)),
            Target::Role(id(1)),
            Target::Role(id(2))
        ]
    );
}

#[test]
fn deduping_nothing_gives_nothing() {
    assert!(dedupe_targets(&[]).is_empty());
}

// --- Target ---------------------------------------------------------------

#[test]
fn a_role_target_has_only_a_role_id() {
    let t = Target::Role(id(7));
    assert_eq!(t.role_id(), Some(id(7)));
    assert_eq!(t.user_id(), None);
}

#[test]
fn a_user_target_has_only_a_user_id() {
    let t = Target::User(id(7));
    assert_eq!(t.user_id(), Some(id(7)));
    assert_eq!(t.role_id(), None);
}

// The two accessors feed the insert's `role_id` and `user_id` binds, and the
// table's check constraint refuses a row where both or neither is set. So
// "exactly one is Some" is the property the schema depends on.
#[test]
fn every_target_sets_exactly_one_column() {
    for t in [Target::Role(id(1)), Target::User(id(2))] {
        assert!(t.role_id().is_some() ^ t.user_id().is_some());
    }
}

// --- Delivery -------------------------------------------------------------

#[test]
fn a_targeted_delivery_with_no_items_is_a_no_op() {
    let d = Delivery::Targeted {
        item_ids: vec![],
        targets: vec![Target::Role(id(1))],
    };
    assert!(d.is_empty());
}

#[test]
fn a_targeted_delivery_with_no_targets_is_a_no_op() {
    let d = Delivery::Targeted {
        item_ids: vec![id(1)],
        targets: vec![],
    };
    assert!(d.is_empty());
}

#[test]
fn a_targeted_delivery_with_both_is_not_a_no_op() {
    let d = Delivery::Targeted {
        item_ids: vec![id(1)],
        targets: vec![Target::User(id(2))],
    };
    assert!(!d.is_empty());
}

#[test]
fn an_invalidation_with_no_targets_is_a_no_op() {
    // The dangerous no-op: an invalidation naming nobody would otherwise wake
    // every client showing a queue to refetch an unchanged answer.
    assert!(Delivery::Invalidate { targets: vec![] }.is_empty());
}

#[test]
fn an_invalidation_naming_a_target_is_not_a_no_op() {
    let d = Delivery::Invalidate {
        targets: vec![Target::Role(id(1))],
    };
    assert!(!d.is_empty());
}

#[test]
fn both_kinds_report_their_targets() {
    let targets = vec![Target::Role(id(1)), Target::User(id(2))];
    let targeted = Delivery::Targeted {
        item_ids: vec![id(9)],
        targets: targets.clone(),
    };
    let invalidate = Delivery::Invalidate {
        targets: targets.clone(),
    };
    assert_eq!(targeted.targets(), targets.as_slice());
    assert_eq!(invalidate.targets(), targets.as_slice());
}

// --- State ----------------------------------------------------------------

#[test]
fn only_pending_is_open() {
    assert!(State::Pending.is_open());
    for s in [State::Resolved, State::Cancelled, State::Expired] {
        assert!(!s.is_open());
    }
}

#[test]
fn every_state_round_trips_through_its_string() {
    // These strings are the check constraint in 0013_action_queue.sql. A state
    // added here without a migration fails this, which is the point.
    for s in [
        State::Pending,
        State::Resolved,
        State::Cancelled,
        State::Expired,
    ] {
        assert_eq!(State::parse(s.as_str()), Some(s));
    }
}

#[test]
fn an_unknown_state_does_not_parse() {
    assert_eq!(State::parse("half-done"), None);
    assert_eq!(State::parse(""), None);
}

// --- serialization --------------------------------------------------------

#[test]
fn a_target_round_trips_through_json() {
    // Targets travel in the NOTIFY payload and out to clients, so the tagged
    // form has to survive the trip.
    for t in [Target::Role(id(4)), Target::User(id(5))] {
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Target>(&json).unwrap(), t);
    }
}

#[test]
fn a_target_serializes_with_its_kind_named() {
    let json = serde_json::to_value(Target::Role(id(4))).unwrap();
    assert_eq!(json["kind"], "role");
}
