use crate::prelude::*;

pub struct MyArgs {
    pub tasksets_dir: String,
    pub output_dir: String,
    pub cgroup: String,
    pub max_num_cpus: u64,
    pub max_allocatable_bw: f32,
    pub num_instances_per_job: u64,
}

pub struct MyResult {
    results: Vec<TasksetRunResult>,
}

#[derive(Debug)]
#[derive(Clone)]
struct Taskset {
    name: String,
    data: Vec<PeriodicTaskData>,
}

#[derive(Debug)]
#[derive(Clone)]
struct TasksetConfig {
    name: String,
    num_cpus: u64,
    runtime_ms: u64,
    period_ms: u64,
}

#[derive(Debug)]
#[derive(Clone)]
struct TasksetRun {
    tasks: Taskset,
    config: TasksetConfig,
    output_file: String,
}

#[derive(Debug)]
#[derive(Clone)]
struct TasksetRunInsights {
    expected_runtime_us: u64
}

#[derive(Debug)]
#[derive(Clone)]
struct TasksetRunResultInstance {
    task: u64,
    instance: u64,
    abs_activation_time_us: u64,
    rel_start_time_us: u64,
    rel_finishing_time_us: u64,
    deadline_offset: f64,
}

#[derive(Debug)]
#[derive(Clone)]
struct TasksetRunResult {
    taskset: Taskset,
    config: TasksetConfig,
    results: Vec<TasksetRunResultInstance>,
}

#[derive(Clone)]
struct TasksetRunResultInsights {
    num_overruns: u64,
    overruns_ratio: f64,
    worst_overrun: f64,
}

mod parser;
use parser::*;

fn __os_str_to_str(string: &std::ffi::OsStr) -> Result<String, Box<dyn std::error::Error>> {
    Ok(
        string.to_os_string().into_string()
            .map_err(|err| format!("Conversion error: {err:?}"))?
    )
}

fn __path_to_str(path: &std::path::Path) -> Result<String, Box<dyn std::error::Error>> {
    __os_str_to_str(path.to_path_buf().as_os_str()) 
}

fn run_taskset(run: TasksetRun, args: &MyArgs)
    -> Result<Option<TasksetRunResult>, Box<dyn std::error::Error>>
{
    if run.config.num_cpus > args.max_num_cpus {
        println!("- Skipping taskset {}, config {}", run.tasks.name, run.config.name);
        println!("  Attempted to run taskset with {0} CPUs on a maximum of {1} CPUs",
            run.config.num_cpus, args.max_num_cpus);
        return Ok(None);
    }

    let taskset_bw = run.config.runtime_ms as f32 / run.config.period_ms as f32;
    if taskset_bw > args.max_allocatable_bw {
        println!("- Skipping taskset {}, config {}", run.tasks.name, run.config.name);
        println!("  Attempted to allocate more bandwidth ({}) than the maximum allocatable ({})",
            taskset_bw, args.max_allocatable_bw);
        return Ok(None);
    }

    let tmp_output_file = "/tmp/out.txt";
    if std::fs::exists(tmp_output_file)? {
        std::fs::remove_file(tmp_output_file)?;
    }

    let cgroup = MyCgroup::new(
        &args.cgroup,
        run.config.runtime_ms * 1000,
        run.config.period_ms * 1000,
        true
    )?;

    migrate_task_to_cgroup(&args.cgroup, std::process::id())?;
    chrt(std::process::id(), MySchedPolicy::RR(99))?;
    set_cpuset_to_pid(std::process::id(), &CpuSet::any_subset(run.config.num_cpus)?)?;

    let pthread_data = PeriodicThreadData {
        start_priority: 98,
        tasks: run.tasks.data.clone(),
        extra_args: String::new(),
        out_file: tmp_output_file.to_owned(),
        num_instances_per_job: args.num_instances_per_job,
    };

    let mut proc = run_periodic_thread(pthread_data)?;
    proc.wait()?;
    
    set_cpuset_to_pid(std::process::id(), &CpuSet::all()?)?;
    chrt(std::process::id(), MySchedPolicy::OTHER)?;
    migrate_task_to_cgroup(".", std::process::id())?;

    cgroup.destroy()?;

    let dirs = std::path::Path::new(&run.output_file).parent()
        .ok_or_else(|| format!("Unknown parent"))?;

    std::fs::create_dir_all(dirs)
        .map_err(|err| format!("Error in creating directory(ies) {dirs:?}: {err}"))?;
    std::fs::copy(tmp_output_file, &run.output_file)
        .map_err(|err| format!("Output file {} copy error: {}", &run.output_file, err))?;

    let result = TasksetRunResult {
        taskset: run.tasks,
        config: run.config,
        results: parse_taskset_results(&run.output_file)?,
    };

    Ok(Some(result))
}

fn compute_insights(run: &TasksetRun, args: &MyArgs) -> TasksetRunInsights {
    let expected_runtime_us = 
        run.tasks.data.iter()
        .map(|task| task.period_ms * args.num_instances_per_job * 1000)
        .max().unwrap();

    TasksetRunInsights { expected_runtime_us }
}

fn compute_result_insights(run: &TasksetRunResult) -> TasksetRunResultInsights {
    let (num_overruns, worst_overrun) = 
        run.results.iter()
        .fold((0u64, f64::NEG_INFINITY), |(mut num_overruns, worst_overrun), job_instance| {
            if job_instance.deadline_offset > 0f64 { num_overruns+= 1; }
            (num_overruns, worst_overrun.max(job_instance.deadline_offset))
        });

    TasksetRunResultInsights {
        num_overruns,
        overruns_ratio: num_overruns as f64 / run.results.len() as f64,
        worst_overrun,
    }
}

pub fn run_taskset_array(args: MyArgs) -> Result<MyResult, Box<dyn std::error::Error>> {
    let tasksets_dir = &args.tasksets_dir;

    let mut taskset_runs = Vec::new();
    for taskset_dir in std::fs::read_dir(&tasksets_dir)
        .map_err(|err| format!("Tasksets directory {} error: {}", &tasksets_dir, err))?
    {
        let taskset_dir = taskset_dir?.path();
        if !taskset_dir.is_dir() {
            continue;
        }

        let files: Vec<String> = std::fs::read_dir(&taskset_dir)
            .map_err(|err| format!("Taskset data directory {:?} error: {}", &taskset_dir, err))?
            .map(|entry| entry.map(|entry| entry.path()))
            .filter(|entry| entry.as_ref().is_ok_and(|entry| entry.is_file()))
            .map(|file| file
                .map_err(|err| Into::<Box<dyn std::error::Error>>::into(err))
                .and_then(|file| file.file_name()
                    .ok_or_else(|| Into::<Box<dyn std::error::Error>>::into(
                        format!("File name not found"))
                    )
                    .and_then(|file| __os_str_to_str(file))
                )
            )
            .try_collect()?;

        let taskset_dir = __path_to_str(taskset_dir.as_path())?;
        if files.iter().find(|file| *file == "taskset.txt").is_none() {
            Err(format!("taskset.txt file not found for taskset {}", taskset_dir))?;
        }

        if files.len() == 1 {
            Err(format!("taskset {} has no run configurations", taskset_dir))?;
        }

        let taskset = parse_taskset_file(&format!("{taskset_dir}/taskset.txt"))?;
        let mut runs: Vec<_> = files.iter().filter(|f| *f != "taskset.txt")
            .map(|config| {
                parse_config_file(&format!("{taskset_dir}/{config}"))
                    .map(|config| {
                        let output_file = format!("{}/{}/output-{}",
                            &args.output_dir, &taskset.name, &config.name);

                        TasksetRun {
                            tasks: taskset.clone(),
                            config,
                            output_file,
                        }
                    })
            })
            .try_collect()?;

        taskset_runs.append(&mut runs);
    }

    let mut failures = 0u64;
    let total_runs = taskset_runs.len() as u64;
    let mut results = Vec::with_capacity(taskset_runs.len());
    for run in taskset_runs.into_iter() {
        let taskset_name = run.tasks.name.clone();
        let config_name = run.config.name.clone();

        let result =
            if std::path::Path::new(&run.output_file).exists() {
                println!("* Skipping taskset {}, config {}: already run",
                    run.tasks.name, run.config.name);

                Ok(Some(TasksetRunResult {
                    taskset: run.tasks,
                    config: run.config,
                    results: parse_taskset_results(&run.output_file)?,
                }))
            } else {
                let insights = compute_insights(&run, &args);
                println!("* Running taskset {}, config {}: expected runtime {:.2} secs",
                    run.tasks.name, run.config.name, insights.expected_runtime_us as f64 / 1000_000f64);

                run_taskset(run, &args)
            }?;

        let Some(result) = result else { continue };
        let insights = compute_result_insights(&result);

        if insights.num_overruns > 0 {
            println!("- Taskset {}, config {} failed: {:.2} % error rate, {} worst overrun",
            taskset_name, config_name, insights.overruns_ratio, insights.worst_overrun);

            failures += 1;
        }

        results.push(result);
    }

    println!("Outcome: {}/{} failures/tests, {:.2} failure ratio",
        failures, total_runs, failures as f64 / total_runs as f64);

    Ok(MyResult { results })
}