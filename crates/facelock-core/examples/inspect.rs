fn main() -> anyhow::Result<()> {
    let env = ort::init_from("/usr/lib/libonnxruntime.so")?.build()?;
    for p in std::env::args().skip(1) {
        let s = ort::session::Session::builder(&env)?.commit_from_file(&p)?;
        println!("== {p}");
        for i in s.inputs() { println!("  in  {:<12} {:?}", i.name(), i.dtype()); }
        for o in s.outputs() { println!("  out {:<12} {:?}", o.name(), o.dtype()); }
    }
    Ok(())
}
