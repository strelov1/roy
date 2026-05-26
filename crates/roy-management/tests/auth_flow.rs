use roy_auth::test_support::temp_pool;
use roy_management::bootstrap::ensure_root;

#[tokio::test]
async fn bootstrap_creates_user_when_table_empty() {
    let pool = temp_pool().await;
    std::env::set_var("ROY_BOOTSTRAP_PASSWORD", "bootstrap-test-pw-1");
    let created = ensure_root(&pool).await.unwrap();
    assert!(created); // first call inserts

    let again = ensure_root(&pool).await.unwrap();
    assert!(!again); // second call is no-op

    let user = roy_auth::UserStore::new(pool.clone())
        .get_by_username("root")
        .await
        .unwrap();
    assert_eq!(user.username, "root");
}
