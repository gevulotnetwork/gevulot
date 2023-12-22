use gevulot_shim::{Task, TaskResult};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    gevulot_shim::run(run_task)
}

fn run_task(task: &Task) -> Result<TaskResult> {
    println!("prover: task.args: {:?}", &task.args);

    std::fs::write("/workspace/proof.dat", b"this is a proof.")?;

    task.result(vec![], vec![String::from("/workspace/proof.dat")])
}
