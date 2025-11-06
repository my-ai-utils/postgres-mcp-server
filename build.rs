fn main() {
    ci_utils::ci_generator::CiGenerator::new(env!("CARGO_PKG_NAME"))
        .as_basic_service()
        .generate_github_ci_file()
        .as_basic_service()
        .build();
}
