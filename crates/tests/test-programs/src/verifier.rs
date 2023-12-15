use gevulot_shim::{Task, TaskResult};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    gevulot_shim::run(run_task)
}

fn run_task(task: &Task) -> Result<TaskResult> {
    println!("verifier: task.args: {:?}", &task.args);

    task.result(vec![], vec![])
}
