fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    corpus_analysis::pareto::run(&arguments)
}
