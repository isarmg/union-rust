mod agent_app;

fn main() -> anyhow::Result<()> {
    agent_app::entry()
}
