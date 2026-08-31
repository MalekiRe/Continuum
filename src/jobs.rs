use crate::executor::{ExecutionResult, Executor, ExecutorError};
use crate::ids::JobId;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Executor(#[from] ExecutorError),
    #[error("unknown background job: {0}")]
    Unknown(JobId),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum JobStatus {
    Running { id: JobId, elapsed_ms: u128 },
    Finished { id: JobId, result: ExecutionResult },
    Failed { id: JobId, error: String },
}

enum Completion {
    Running,
    Finished(Result<ExecutionResult, String>),
}

struct Job {
    executor: Executor,
    started: Instant,
    completion: Arc<Mutex<Completion>>,
}

#[derive(Clone)]
pub struct JobManager {
    template: Executor,
    jobs: Arc<Mutex<HashMap<JobId, Job>>>,
}

impl JobManager {
    pub fn new(template: Executor) -> Self {
        Self {
            template,
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn start(&self, command: String) -> Result<JobId, JobError> {
        let id = JobId::new(format!("job-{}", uuid::Uuid::new_v4()));
        let executor = self.template.independent()?;
        let completion = Arc::new(Mutex::new(Completion::Running));
        self.jobs.lock().unwrap().insert(
            id.clone(),
            Job {
                executor: executor.clone(),
                started: Instant::now(),
                completion: completion.clone(),
            },
        );
        std::thread::spawn(move || {
            let result = executor.run(&command).map_err(|error| error.to_string());
            *completion.lock().unwrap() = Completion::Finished(result);
        });
        Ok(id)
    }

    pub fn status(&self, id: &JobId) -> Result<JobStatus, JobError> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs.get(id).ok_or_else(|| JobError::Unknown(id.clone()))?;
        let completion = job.completion.lock().unwrap();
        Ok(match &*completion {
            Completion::Running => JobStatus::Running {
                id: id.clone(),
                elapsed_ms: job.started.elapsed().as_millis(),
            },
            Completion::Finished(Ok(result)) => JobStatus::Finished {
                id: id.clone(),
                result: result.clone(),
            },
            Completion::Finished(Err(error)) => JobStatus::Failed {
                id: id.clone(),
                error: error.clone(),
            },
        })
    }

    pub fn cancel(&self, id: &JobId) -> Result<bool, JobError> {
        let jobs = self.jobs.lock().unwrap();
        let job = jobs.get(id).ok_or_else(|| JobError::Unknown(id.clone()))?;
        Ok(job.executor.cancel()?)
    }

    pub fn collect(&self, id: &JobId) -> Result<JobStatus, JobError> {
        let status = self.status(id)?;
        if !matches!(status, JobStatus::Running { .. }) {
            self.jobs.lock().unwrap().remove(id);
        }
        Ok(status)
    }

    pub fn list(&self) -> Vec<JobStatus> {
        let ids: Vec<_> = self.jobs.lock().unwrap().keys().cloned().collect();
        ids.iter().filter_map(|id| self.status(id).ok()).collect()
    }

    pub fn cancel_all(&self) -> Result<(), JobError> {
        let executors: Vec<_> = self
            .jobs
            .lock()
            .unwrap()
            .values()
            .map(|job| job.executor.clone())
            .collect();
        for executor in executors {
            executor.cancel()?;
        }
        Ok(())
    }
}
