//! マイグレーションはコンパイル時に埋め込むので、migrations/ が変わったら作り直す。
fn main() {
    println!("cargo:rerun-if-changed=../../migrations");
}
