use hcbs_test_suite::prelude::*;

#[derive(clap::Parser, Debug)]
pub struct MyArgs {
    /// cgroup's name
    #[arg(short = 'c', long = "cgroup", default_value = "g0", value_name = "name")]
    pub cgroup: String,

    /// cgroup's first runtime
    #[arg(short = 'r', long = "runtime1", value_name = "ms: u64")]
    pub runtime1_ms: u64,

    /// cgroup's second runtime
    #[arg(short = 'R', long = "runtime2", value_name = "ms: u64")]
    pub runtime2_ms: u64,

    /// cgroup's period
    #[arg(short = 'p', long = "period", value_name = "ms: u64")]
    pub period_ms: u64,

    /// pinning change period
    #[arg(short = 'P', long = "change-period", value_name = "secs: f32")]
    pub change_period: f32,

    /// max running time
    #[arg(short = 't', long = "max-time", value_name = "sec: u64")]
    pub max_time: Option<u64>,
}

pub fn main(args: MyArgs, ctrlc_flag: Option<ExitFlag>) -> Result<(), Box<dyn std::error::Error>> {
    let mut cgroup = MyCgroup::new(&args.cgroup, args.runtime1_ms * 1000, args.period_ms * 1000, true)?;
    migrate_task_to_cgroup(&args.cgroup, std::process::id())?;
    chrt(std::process::id(), MySchedPolicy::RR(99))?;

    let mut proc = run_yes()?;
    let mut state = args.runtime1_ms;

    let update_fn = || {
        if state == args.runtime1_ms {
            state = args.runtime2_ms;
        } else {
            state = args.runtime1_ms;
        }

        cgroup.update_runtime(state)?;
        Ok(())
    };

    if !is_batch_test() {
        println!("Started Yes process\nPress Ctrl+C to stop");
    }

    wait_loop_periodic_fn(args.change_period, args.max_time, ctrlc_flag, update_fn)?;

    proc.kill()?;
    chrt(std::process::id(), MySchedPolicy::OTHER)?;
    migrate_task_to_cgroup(".", std::process::id())?;
    cgroup.destroy()?;

    Ok(())
}