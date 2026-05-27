//! Gate del Paso 4 (docs/08-getting-started.md): el **mismo** contrato corre contra dos
//! impls del puerto `BeadRepository`:
//!   1. `InMemoryBeads` — siempre (host, sin BD).
//!   2. `DoltBeads` — contra un Dolt real, si `GT_DOLT_URL` está seteado (se valida dentro
//!      del contenedor). Si uno pasa y el otro no, el puerto está mal definido.

use gt_beads::{Bead, BeadRepository, BeadStatus, InMemoryBeads};
use gt_store_dolt::DoltBeads;

/// Aserciones del contrato, agnósticas de la impl. Asume tabla/slate limpio.
async fn bead_repo_contract<R: BeadRepository>(repo: &R) {
    // upsert + get
    repo.upsert(&Bead::new("b1", "first", BeadStatus::Pending, 1)).await.unwrap();
    repo.upsert(&Bead::new("b2", "second", BeadStatus::Pending, 0)).await.unwrap();
    assert_eq!(repo.get("b1").await.unwrap().unwrap().title, "first");
    assert!(repo.get("nope").await.unwrap().is_none());

    // cola = vista por estado
    assert_eq!(repo.list_by_status(BeadStatus::Pending).await.unwrap().len(), 2);

    // CAS claim: el primero gana, el segundo pierde (ya no está pending)
    assert!(repo.cas_claim("b1", "worker-a").await.unwrap(), "primer claim gana");
    assert!(!repo.cas_claim("b1", "worker-b").await.unwrap(), "segundo claim pierde");

    let b1 = repo.get("b1").await.unwrap().unwrap();
    assert_eq!(b1.status, BeadStatus::Dispatched);
    assert_eq!(b1.assignee.as_deref(), Some("worker-a"));

    // tras el claim, solo b2 sigue pending
    assert_eq!(repo.list_by_status(BeadStatus::Pending).await.unwrap().len(), 1);

    // upsert sobre-escribe (idempotente por id)
    repo.upsert(&Bead::new("b2", "second-v2", BeadStatus::Pending, 0)).await.unwrap();
    assert_eq!(repo.get("b2").await.unwrap().unwrap().title, "second-v2");

    // CAS release (visibility-timeout / lease-expired): sólo gana si el bead sigue
    // dispatched **Y** asignado al expected_worker.
    assert!(
        !repo.cas_release("b1", "worker-b").await.unwrap(),
        "release con worker incorrecto pierde",
    );
    let b1 = repo.get("b1").await.unwrap().unwrap();
    assert_eq!(b1.status, BeadStatus::Dispatched, "estado intacto tras release perdido");

    assert!(repo.cas_release("b1", "worker-a").await.unwrap());
    let b1 = repo.get("b1").await.unwrap().unwrap();
    assert_eq!(b1.status, BeadStatus::Pending);
    assert_eq!(b1.assignee, None);

    // segundo release sobre un bead ya pending pierde (no es dispatched).
    assert!(!repo.cas_release("b1", "worker-a").await.unwrap());

    // tras release, b1 vuelve a ser reclamable por otro worker.
    assert!(repo.cas_claim("b1", "worker-c").await.unwrap());
    assert_eq!(
        repo.get("b1").await.unwrap().unwrap().assignee.as_deref(),
        Some("worker-c"),
    );
}

#[tokio::test]
async fn contract_in_memory() {
    let repo = InMemoryBeads::default();
    bead_repo_contract(&repo).await;
}

#[tokio::test]
async fn contract_dolt() {
    // GT_DOLT_URL = URL base del server SIN base de datos, p. ej.
    // "mysql://root@127.0.0.1:3307". Si no está, se omite (host sin Dolt).
    let Ok(base) = std::env::var("GT_DOLT_URL") else {
        eprintln!("GT_DOLT_URL no seteado — se omite el contrato Dolt (correr dentro del contenedor)");
        return;
    };
    let base = base.trim_end_matches('/').to_string();

    // Bootstrap: crear la BD de test vía el propio cliente MySQL-wire (no hace falta dolt CLI).
    {
        use mysql_async::prelude::Queryable;
        let pool = gt_store_dolt::connect(&base).expect("conectar al server");
        let mut conn = pool.get_conn().await.expect("conn admin");
        conn.query_drop("CREATE DATABASE IF NOT EXISTS gt_rs_test")
            .await
            .expect("crear gt_rs_test");
    }

    let repo = DoltBeads::connect(&format!("{base}/gt_rs_test")).expect("conectar a gt_rs_test");
    repo.ensure_schema().await.expect("crear schema");
    repo.truncate().await.expect("limpiar tabla");
    bead_repo_contract(&repo).await;
}
