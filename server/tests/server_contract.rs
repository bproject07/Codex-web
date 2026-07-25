use codex_web_terminal::{
    auth::token_matches,
    protocol::{ClientControl, parse_control_message, validate_resize},
    session::BoundedOutputBuffer,
};
use uuid::Uuid;

#[test]
fn public_protocol_and_auth_contract_is_stable() {
    assert!(token_matches("0123456789abcdef", "0123456789abcdef"));
    assert_eq!(
        parse_control_message(r#"{"type":"resize","cols":80,"rows":24}"#)
            .expect("valid control message"),
        ClientControl::Resize { cols: 80, rows: 24 }
    );
    assert!(validate_resize(80, 24).is_ok());
    assert!(validate_resize(10, 1).is_err());
}

#[test]
fn reconnect_buffer_exposes_only_the_active_session() {
    let first_session = Uuid::new_v4();
    let second_session = Uuid::new_v4();
    let mut buffer = BoundedOutputBuffer::new(32);

    buffer.reset(first_session);
    buffer.append(first_session, b"first");
    buffer.reset(second_session);
    buffer.append(first_session, b"stale");
    buffer.append(second_session, b"current");

    let snapshot = buffer.snapshot();
    assert_eq!(snapshot.session_id, Some(second_session));
    assert_eq!(snapshot.chunks.len(), 1);
    assert_eq!(&snapshot.chunks[0].data[..], b"current");
}
