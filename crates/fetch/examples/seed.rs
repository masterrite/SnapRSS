// Populates a database from an OPML file and fetches every feed once.
//   cargo run -p snaprss-fetch --example seed -- <db> <opml>
#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut a = std::env::args().skip(1);
    let db_path = a.next().unwrap();
    let opml = a.next().unwrap();

    let mut db = snaprss_core::Db::open(&db_path).unwrap();
    let (_, r) = snaprss_core::import::import_any(&mut db, &opml).unwrap();
    println!("imported {} feeds, {} duplicates", r.feeds, r.duplicates);

    let cfg = snaprss_fetch::FetchConfig::default();
    let client = snaprss_fetch::build_client(&cfg).unwrap();
    let s = snaprss_fetch::update_due(&mut db, &client, &cfg, 15, 8)
        .await
        .unwrap();
    println!("{:?}", s);
    println!("unread now {}", db.total_unread().unwrap());
}
