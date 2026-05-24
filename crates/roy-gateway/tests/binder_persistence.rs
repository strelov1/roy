use roy_gateway::binder::SessionBinder;

#[tokio::test]
async fn bindings_survive_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("b.json");

    {
        let binder = SessionBinder::load(path.clone()).await.unwrap();
        binder.set(1, "alpha".into()).await.unwrap();
        binder.set(2, "beta".into()).await.unwrap();
    }

    let reloaded = SessionBinder::load(path).await.unwrap();
    assert_eq!(reloaded.get(1).await.as_deref(), Some("alpha"));
    assert_eq!(reloaded.get(2).await.as_deref(), Some("beta"));
}
