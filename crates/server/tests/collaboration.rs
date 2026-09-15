//! APP-009 ordering and membership revoke.

use quansio_server::control::collaboration::{can_receive, order_messages, OrderedMessage};

#[test]
fn concurrent_messages_have_stable_order() {
    let messages = vec![
        OrderedMessage {
            id: "msg_01J8Z3K6F1N8VQ2X5W9Y0BBBBB".into(),
            seq: 2,
        },
        OrderedMessage {
            id: "msg_01J8Z3K6F1N8VQ2X5W9Y0AAAAA".into(),
            seq: 1,
        },
        OrderedMessage {
            id: "msg_01J8Z3K6F1N8VQ2X5W9Y0CCCCC".into(),
            seq: 2,
        },
    ];
    let ordered = order_messages(messages);
    assert_eq!(ordered[0].seq, 1);
    assert_eq!(ordered[1].id, "msg_01J8Z3K6F1N8VQ2X5W9Y0BBBBB");
    assert_eq!(ordered[2].id, "msg_01J8Z3K6F1N8VQ2X5W9Y0CCCCC");
    let again = order_messages(ordered.clone());
    assert_eq!(ordered, again);
}

#[test]
fn removed_participant_cannot_receive_protected_events() {
    assert!(can_receive("active"));
    assert!(!can_receive("removed"));
    assert!(!can_receive("suspended"));
}
