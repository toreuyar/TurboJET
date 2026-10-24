FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && apt-get install -y --no-install-recommends \
    git \
    curl \
    ca-certificates \
    jq \
    nodejs \
    npm \
    gosu \
  && rm -rf /var/lib/apt/lists/*

RUN npm install -g opencode-ai @ai-sdk/openai-compatible

RUN if id -u 1000 >/dev/null 2>&1; then \
      existing_user=$(getent passwd 1000 | cut -d: -f1); \
      usermod -l opencode -d /home/opencode -m "$existing_user" 2>/dev/null || true; \
      groupmod -n opencode "$(getent group 1000 | cut -d: -f1)" 2>/dev/null || true; \
    else \
      groupadd -g 1000 opencode && \
      useradd -m -u 1000 -g 1000 -s /bin/bash opencode; \
    fi && \
    mkdir -p /home/opencode/.config \
    /home/opencode/worktree \
    /home/opencode/output \
    && chown -R opencode:opencode /home/opencode

COPY entrypoint.sh /usr/local/bin/entrypoint.sh

WORKDIR /home/opencode
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["bash"]
