fn main() {
    // `sqlx::migrate!` embeds every file in this directory; make cargo notice new ones.
    println!("cargo:rerun-if-changed=migrations");
}
