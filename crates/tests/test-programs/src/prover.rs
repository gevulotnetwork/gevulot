use gevulot_shim::{Task, TaskResult};
use std::fs;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    gevulot_shim::run(run_task)
}

fn run_task(task: &Task) -> Result<TaskResult> {
    println!("prover: task.args: {:?}", &task.args);

    //TODO define some provided context
    let files = task.get_task_files_path("/workspace");
    println!("Prover: get nb files:{}", files.len());
    for (name, path) in files {
        let content = String::from_utf8(std::fs::read(path)?)?;
        println!("Prover: Read file:{name} with content:{content:?}");
    }
    let entries = fs::read_dir("/workspace")
        .unwrap()
        .map(|res| res.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    println!("file entries in /workspace :: {:?}", entries);

    std::fs::write("/workspace/proof.dat", b"this is a proof.")?;

    task.result(
        vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9],
        vec![String::from("/workspace/proof.dat")],
    )
}
