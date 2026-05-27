//! Gate del Paso 5 (docs/08-getting-started.md): flujo orquestado real con DOS dominios +
//! Dolt-port (in-memory) + replay. enqueue → CAS-claim → dispatch → spawn (gt-agent) →
//! completion. El replay del log reconstruye el mismo flujo.
//!
//! Este test juega el rol de **composition root**: cablea scheduling + agent y reacciona a
//! los eventos del relay (lo que en producción hace `bins/gt`).

use std::sync::Arc;

use tokio::sync::mpsc;

use gt_agent::{actor as agent_actor, Session, SessionState};
use gt_audit::{read_all, replay, EventRecord, EventStore, JsonlWriter};
use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_events::Envelope;
use gt_scheduling::{actor as sched, SchedEvent, SchedState};

#[tokio::test]
async fn orchestrated_flow_enqueue_to_completion() {
    // Repo compartido (in-memory port; el contrato ya pasó contra Dolt en Paso 4).
    let repo = Arc::new(InMemoryBeads::default());
    for (id, p) in [("b2", 2u8), ("b0", 0u8), ("b1", 1u8)] {
        repo.upsert(&Bead::new(id, "work", BeadStatus::Pending, p)).await.unwrap();
    }

    let (ev_tx, mut ev_rx) = mpsc::channel::<Envelope<SchedEvent>>(64);
    // Capacidad 1 → solo uno en vuelo: fuerza a que la cola ordene por prioridad.
    let dispatcher = sched::spawn(repo.clone(), ev_tx, 1);
    let agent = agent_actor::spawn(32);

    let log_path = std::env::temp_dir().join(format!("gt-orch-{}.events.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&log_path);
    let writer = JsonlWriter::new(&log_path);

    // Encola en orden de llegada b2, b0, b1 (prioridades 2,0,1).
    dispatcher.enqueue("b2", 2).await;
    dispatcher.enqueue("b0", 0).await;
    dispatcher.enqueue("b1", 1).await;

    let mut dispatch_order = Vec::new();
    let mut completed = 0;
    while completed < 3 {
        let env = ev_rx.recv().await.expect("evento de scheduling");
        writer.append(&EventRecord::from_envelope(&env).unwrap()).unwrap();

        if let SchedEvent::Dispatched { bead, .. } = &env.payload {
            dispatch_order.push(bead.clone());

            // "spawn vía gt-agent" — la sesión representa el polecat (spawn real: Paso 2).
            agent.add(Session::new(bead.clone(), "granite")).await;
            agent.transition(bead.clone(), SessionState::Working).await.unwrap();
            agent.transition(bead.clone(), SessionState::Done).await.unwrap();

            // completion: el bead pasa a done en el repo.
            if let Some(mut b) = repo.get(bead).await.unwrap() {
                b.status = BeadStatus::Done;
                repo.upsert(&b).await.unwrap();
            }
            completed += 1;
            dispatcher.capacity_freed().await; // libera → desencola el siguiente
        }
    }

    // b2 sale primero (llegó con capacidad libre); luego la cola ordena P0 antes que P1.
    assert_eq!(dispatch_order, vec!["b2", "b0", "b1"]);

    // las 3 sesiones quedan terminales en el agent.
    let snap = agent.snapshot().await;
    assert_eq!(snap.len(), 3);
    assert!(snap.iter().all(|s| s.is_terminal()));

    // capacidad de vuelta a 0, cola vacía.
    let (queued, in_flight) = dispatcher.snapshot().await;
    assert_eq!((queued, in_flight), (0, 0));

    // ningún bead sigue pending.
    assert!(repo.list_by_status(BeadStatus::Pending).await.unwrap().is_empty());

    // REPLAY: re-correr el log reconstruye el flujo idéntico.
    let records = read_all(&log_path).unwrap();
    let state = replay(&records, SchedState::default(), |s, e: &SchedEvent| s.apply(e)).unwrap();
    assert_eq!(state.enqueued.len(), 3);
    assert_eq!(state.dispatched.len(), 3);
    assert!(state.failed.is_empty());
    let replayed_order: Vec<&str> = state.dispatched.iter().map(|(b, _)| b.as_str()).collect();
    assert_eq!(replayed_order, vec!["b2", "b0", "b1"]);

    let _ = std::fs::remove_file(&log_path);
}
