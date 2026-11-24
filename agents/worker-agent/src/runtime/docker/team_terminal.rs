use super::*;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use rsctf_worker_protocol::InteractiveExecRequest;
use tokio::io::{AsyncWriteExt, DuplexStream};

impl DockerRuntime {
    pub(super) async fn team_terminal(
        &self,
        request: &InteractiveExecRequest,
    ) -> Result<DuplexStream, RuntimeError> {
        if self.platform.operating_system != OperatingSystem::Linux {
            return Err(RuntimeError::unsupported("team terminals require Linux"));
        }
        self.flag_delivery.check_team_terminal(request).await?;
        let targets = self
            .matching_container_ids(request.fence, &request.service)
            .await?;
        let [(0, id)] = targets.as_slice() else {
            return Err(RuntimeError::new(
                CommandErrorCode::NotFound,
                "team service is not ready",
            ));
        };
        let exec = self
            .admission
            .lifecycle(
                "create_team_terminal",
                self.docker.create_exec(
                    id,
                    CreateExecOptions {
                        attach_stdin: Some(true),
                        attach_stdout: Some(true),
                        attach_stderr: Some(true),
                        tty: Some(true),
                        privileged: Some(false),
                        cmd: Some(vec!["/bin/sh".to_owned()]),
                        env: Some(vec![
                            "TERM=xterm-256color".to_owned(),
                            format!("COLUMNS={}", request.columns),
                            format!("LINES={}", request.rows),
                        ]),
                        ..Default::default()
                    },
                ),
            )
            .await?
            .map_err(|e| docker_error("open team terminal", e))?;
        let started = self
            .admission
            .lifecycle(
                "start_team_terminal",
                self.docker.start_exec(
                    &exec.id,
                    Some(StartExecOptions {
                        tty: true,
                        ..Default::default()
                    }),
                ),
            )
            .await?
            .map_err(|e| docker_error("attach team terminal", e))?;
        let StartExecResults::Attached {
            mut output,
            mut input,
        } = started
        else {
            return Err(RuntimeError::new(
                CommandErrorCode::Internal,
                "team terminal did not attach",
            ));
        };
        let (client, pump) = tokio::io::duplex(8192);
        let (mut rd, mut wr) = tokio::io::split(pump);
        tokio::spawn(async move {
            let send = async {
                while let Some(Ok(message)) = output.next().await {
                    if wr.write_all(&message.into_bytes()).await.is_err() {
                        break;
                    }
                }
            };
            tokio::select! {
                _ = tokio::io::copy(&mut rd, &mut input) => {},
                _ = send => {},
            }
            let _ = input.shutdown().await;
        });
        Ok(client)
    }
}
