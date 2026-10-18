use chat_core::p0::{P0Session, Side};
use std::sync::Arc;

#[test]
fn real_iroh_mls_bidirectional_and_lifecycle() {
    let session = P0Session::new().unwrap();
    let info = session.info().unwrap();
    assert_ne!(info.alice_endpoint, info.bob_endpoint);
    assert!(!info.public_infrastructure);
    assert_eq!(info.epoch, 1);
    assert!(session.send(Side::Alice, " ").is_err());
    assert!(session.send(Side::Alice, &"x".repeat(8193)).is_err());
    for i in 0..3 {
        let message = format!("真实消息 {i} 👋");
        let result = session
            .send(if i % 2 == 0 { Side::Alice } else { Side::Bob }, &message)
            .unwrap();
        assert_eq!(result.text, message);
        assert_eq!(result.exchange_index, i + 1);
        assert!(result.ciphertext_bytes as usize > message.len());
        assert_eq!(result.ciphertext_sha256.len(), 64);
    }
    session.close().unwrap();
    session.close().unwrap();
    assert!(session.send(Side::Alice, "closed").is_err());
    assert!(session.info().is_err());
}

#[test]
fn concurrent_calls_are_serialized_without_reusing_mls_state() {
    let session = Arc::new(P0Session::new().unwrap());
    let workers: Vec<_> = [Side::Alice, Side::Bob]
        .into_iter()
        .map(|side| {
            let session = session.clone();
            std::thread::spawn(move || session.send(side, side.name()).unwrap())
        })
        .collect();
    let mut indices: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().exchange_index)
        .collect();
    indices.sort();
    assert_eq!(indices, vec![1, 2]);
    session.close().unwrap();
}
