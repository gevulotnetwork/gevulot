use gevulot_common::WORKSPACE_PATH;
use gevulot_shim::{Task, TaskResult};
use std::fs;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    gevulot_shim::run(run_task)
}

fn run_task(task: Task) -> Result<TaskResult> {
    println!("verifier: task.args: {:?}", &task.args);

    let files = task.get_task_files_path(WORKSPACE_PATH);
    println!("Prover: get nb files:{}", files.len());
    for (name, _path) in files {
        //let content = String::from_utf8(std::fs::read(path)?)?;
        //println!("Prover: Read file:{name} with content:{content:?}");
        println!("Prover: Read file:{name}");
    }

    let entries = fs::read_dir(WORKSPACE_PATH)
        .unwrap()
        .map(|res| res.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()
        .unwrap();
    println!("file entries in /workspace :: {:?}", entries);

    task.result(vec![10, 11, 12, 13, 14, 15, 16, 17, 18, 19], vec![])
}
