//! Output captured on the GPU host, with the user's name written `<user>`, for the survey's tests.

/// `nvidia-smi --query-gpu=memory.used,memory.total --format=csv,noheader`, nothing loaded.
pub(crate) const IDLE_TOTALS: &str = "17 MiB, 24564 MiB\n";

/// `nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader` with
/// Strata loaded. Its parent chain is python, conmon, systemd: not its sheep's tree.
pub(crate) const STRATA_ENGINE: &str = "188622, /opt/strata/engine/strata, 23702 MiB\n";

/// The same query with qwen loaded: ollama's runner.
pub(crate) const QWEN_RUNNER_APP: &str =
    "190784, /home/<user>/.local/opt/ollama/lib/ollama/llama-server, 19542 MiB\n";

/// The runner's pid in [`QWEN_RUNNER_APP`].
pub(crate) const QWEN_RUNNER_PID: u32 = 190_784;

/// `/proc/190784/cmdline`, its arguments joined by single spaces.
pub(crate) const QWEN_RUNNER_CMDLINE: &str = "/home/<user>/.local/opt/ollama/lib/ollama/llama-server \
    --model /home/<user>/.ollama/models/blobs/sha256-f5f1dd8920d417aac2718b0bda3403da274301efdd6760b4f0f4b864ff2ad57d \
    --port 37243 --host 127.0.0.1 --no-webui --offline -c 65536 -np 1 --log-verbosity 4 \
    --no-log-prefix --no-log-timestamps --no-jinja --chat-template chatml --spec-type draft-mtp \
    --spec-draft-n-max 4 --spec-draft-backend-sampling --cache-type-k q8_0 --cache-type-v q8_0 \
    --flash-attn on -b 512 -ub 512 -t 4 --context-shift --keep 4";

/// The model blob `/api/show`'s modelfile names for qwen, which the runner loads.
pub(crate) const QWEN_BLOB: &str =
    "f5f1dd8920d417aac2718b0bda3403da274301efdd6760b4f0f4b864ff2ad57d";

/// The manifest digest `/api/ps` gives for the same model. No command line names it.
pub(crate) const QWEN_MANIFEST: &str =
    "b94f869e259e93f78713b68f6bcce55add05404516c050e07e57033b5f0157d7";

/// `GET /api/ps` with qwen loaded.
pub(crate) const PS_QWEN: &str = r#"{"models":[{"name":"qwen3.8:27b-ctx65536","model":"qwen3.8:27b-ctx65536","digest":"b94f869e259e93f78713b68f6bcce55add05404516c050e07e57033b5f0157d7","size":17275897773,"size_vram":17275897773,"context_length":65536}]}"#;

/// `POST /api/show {"model": "qwen3.8:27b-ctx65536"}`, its `modelfile` cut to the `FROM` line.
pub(crate) const SHOW_QWEN: &str = r#"{"modelfile":"FROM /home/<user>/.ollama/models/blobs/sha256-f5f1dd8920d417aac2718b0bda3403da274301efdd6760b4f0f4b864ff2ad57d\n"}"#;

/// [`QWEN_RUNNER_CMDLINE`] as the argument list `/proc` holds. No captured argument has a space.
pub(crate) fn qwen_runner_args() -> Vec<String> {
    QWEN_RUNNER_CMDLINE.split(' ').map(str::to_owned).collect()
}
