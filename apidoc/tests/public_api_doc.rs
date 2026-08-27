//! Public-API surface snapshots for every zenextras workspace member
//! (zentiff, zensvg, zenjp2, zenpdf) under docs/public-api/. Shared
//! implementation + format docs: the `zenutils-apidoc` crate.
#[test]
fn public_api_surface_docs_are_current() {
    zenutils_apidoc::ApiDoc::new().workspace_dir("..").run();
}
