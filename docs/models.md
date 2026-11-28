# Models

GenAImage never downloads a model by itself. Not at startup, when you save
settings, in `doctor`, or after a "model not found" error. An LLM model is
pulled only when you ask: `genaimage models pull <name>` or the portal's pull
action. ComfyUI checkpoints are never downloaded by GenAImage at all; you copy
the files yourself.

## LLM models (Ollama)

`genaimage models list` and `GET /api/v1/models/llm` show what is installed and
whether the configured text and vision models are present. An absent model is
reported as missing; nothing is substituted.

Pulling needs Ollama's native API. GenAImage derives its address from
`llm.base_url` by removing a trailing `/v1`:

| `llm.base_url` | Native API |
| --- | --- |
| `http://127.0.0.1:11434/v1` | `http://127.0.0.1:11434` |
| `http://ollama:11434/v1` (Compose) | `http://ollama:11434` |
| anything not ending in `/v1` | not derived, so pull is unavailable |

The same endpoint rules apply as for chat: loopback by default, other private
or Compose hosts only when listed in `llm.allowed_hosts`. For llama.cpp, LM
Studio and other OpenAI-compatible servers, listing falls back to `/v1/models`
and pulling is reported as unsupported. Install models with that server's own
tooling.

Pulls are server-side jobs held in memory:

- At most one pull per model name (the tag `:latest` is implied) and at most two
  at a time.
- A client can leave and re-attach with `GET /api/v1/models/llm/pulls/{id}` or
  its `/events` stream, for example after a page reload.
- They are not saved. After a server restart a pull ID is unknown (`404`),
  which never means success; list the models to see what arrived.
- A stream that ends without Ollama's `success` is a failure. Idle streams and
  pulls running longer than six hours are stopped and reported as failed.
- `DELETE /api/v1/models/llm/pulls/{id}` cancels a running pull. Ollama keeps
  partial layers, so a later pull resumes. The request may have finished just
  before it closed, so a cancelled pull can still have installed the model;
  `GET /api/v1/models/llm` is authoritative.
- A pull whose request or task ends unexpectedly is recorded as failed and
  frees its model name and slot.
- Pulls tolerate up to ten minutes of silence between progress lines, because
  Ollama can stay quiet while it verifies a large layer. Progress tracks at
  most 256 layers.
- Names are only canonicalised for the implied `:latest` tag. `llama3` and
  `library/llama3` count as different names; Ollama deduplicates their layers,
  so the second pull is cheap.

Starter models are listed in the [Ollama library](https://ollama.com/library).
For a text assistant with tool calling, look at the models tagged "tools"; for
vision review, the ones tagged "vision". Pick one that fits your memory and set
`llm.text_model` and `llm.vision_model`.

## ComfyUI checkpoints

`genaimage models list` and `GET /api/v1/models/checkpoints` list the
`ckpt_name` options of ComfyUI's `CheckpointLoaderSimple`. If ComfyUI is down or
lacks that node, the answer is an error, not an empty list.

The API response also maps every checkpoint name to the pack
[checkpoint profiles](packs.md#checkpoint-profiles) that list exactly that name.
An empty list means that no profile matches; a model family is never inferred
from the file name.

Put checkpoint files here:

- Docker Compose: `/opt/comfyui/models/checkpoints` in the
  `genaimage_comfyui-models` volume (see [Compose](compose.md)). Copy a file in
  with `docker compose cp <file> comfyui:/opt/comfyui/models/checkpoints/`.
  Under Compose you can also pull an LLM model with
  `docker compose exec ollama ollama pull <model>`; the Ollama native API is
  `http://ollama:11434`.
- Native ComfyUI: `<ComfyUI>/models/checkpoints`.

ComfyUI only sees new files after a refresh (press `r` in its web UI) or a
restart. Check the licence of each checkpoint before use.

Starter checkpoints (links only; download them yourself from the model page):

- [Stable Diffusion XL base 1.0](https://huggingface.co/stabilityai/stable-diffusion-xl-base-1.0)
- [Stable Diffusion 1.5](https://huggingface.co/stable-diffusion-v1-5/stable-diffusion-v1-5)
- [FLUX.1 schnell](https://huggingface.co/black-forest-labs/FLUX.1-schnell)
- [Browse Hugging Face text-to-image models](https://huggingface.co/models?pipeline_tag=text-to-image)
