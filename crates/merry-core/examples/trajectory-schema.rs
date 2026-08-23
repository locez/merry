use merry_core::TrajectoryEvent;

fn main() {
    let schema = schemars::schema_for!(TrajectoryEvent);
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).expect("trajectory schema serializes")
    );
}
