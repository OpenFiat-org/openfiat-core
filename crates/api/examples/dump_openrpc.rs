//! Regenerates the static OpenRPC snapshot published in `openfiat-docs`
//! (which has no live node to serve `/openrpc.json` from):
//!
//! ```sh
//! cargo run -p openfiat-api --example dump_openrpc > openrpc.json
//! ```
//!
//! Re-run this and copy the result into `openfiat-docs/static/api/
//! openrpc.json` whenever `openfiat-rpc`'s method table changes.

fn main() {
    let document = openfiat_api::openrpc::build_document();
    println!(
        "{}",
        serde_json::to_string_pretty(&document).expect("OpenRPC document always serializes")
    );
}
