ARG PYTHON_IMAGE=docker.io/library/python:3.12-slim-bookworm@sha256:44cf1793efa7c7a476bb0f2cff81443f4a072c90c1cb127e2924933daa3e8c64

FROM ${PYTHON_IMAGE} AS build
ARG COMFYUI_TAG=v0.46.0
ARG COMFYUI_COMMIT=a277b5bf213e8a44fa34817b7770b54f86ef4cea
ARG TORCH_VERSION=2.14.1
ARG TORCHVISION_VERSION=0.29.1
ARG TORCHAUDIO_VERSION=2.11.0
ARG TORCH_INDEX=cpu
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN git clone --depth 1 --branch "${COMFYUI_TAG}" https://github.com/Comfy-Org/ComfyUI.git /opt/comfyui \
    && test "$(git -C /opt/comfyui rev-parse HEAD)" = "${COMFYUI_COMMIT}" \
    && rm -rf /opt/comfyui/.git
RUN python -m venv /opt/venv
ENV PATH=/opt/venv/bin:$PATH PIP_NO_CACHE_DIR=1
RUN pip install "torch==${TORCH_VERSION}" "torchvision==${TORCHVISION_VERSION}" "torchaudio==${TORCHAUDIO_VERSION}" \
        --index-url "https://download.pytorch.org/whl/${TORCH_INDEX}" \
    && printf 'torch==%s\ntorchvision==%s\ntorchaudio==%s\n' "${TORCH_VERSION}" "${TORCHVISION_VERSION}" "${TORCHAUDIO_VERSION}" > /tmp/torch-constraints.txt \
    && pip install -c /tmp/torch-constraints.txt -r /opt/comfyui/requirements.txt \
    && python -c "import torch, torchvision, torchaudio"

FROM ${PYTHON_IMAGE}
COPY --from=build /opt/venv /opt/venv
COPY --from=build --chown=65532:65532 /opt/comfyui /opt/comfyui
COPY --chmod=755 entrypoint.sh /usr/local/bin/comfyui-entrypoint
ENV PATH=/opt/venv/bin:$PATH \
    PYTHONUNBUFFERED=1 \
    HOME=/opt/comfyui/user
WORKDIR /opt/comfyui
RUN mkdir -p input output user temp models custom_nodes \
    && chown -R 65532:65532 /opt/comfyui
VOLUME ["/opt/comfyui/models", "/opt/comfyui/custom_nodes", "/opt/comfyui/input", "/opt/comfyui/output", "/opt/comfyui/user"]
EXPOSE 8188
USER 65532:65532
HEALTHCHECK --interval=10s --timeout=5s --start-period=60s --retries=6 \
    CMD ["python", "-c", "import urllib.request; urllib.request.urlopen('http://127.0.0.1:8188/system_stats', timeout=3)"]
ENTRYPOINT ["comfyui-entrypoint"]
CMD ["python", "main.py", "--listen", "0.0.0.0", "--port", "8188"]
