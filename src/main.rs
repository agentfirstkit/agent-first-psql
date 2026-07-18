#[tokio::main]
async fn main() {
    agent_first_psql::run(agent_first_psql::Capability::ReadWrite, "afpsql").await;
}
