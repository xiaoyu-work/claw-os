FROM scratch

COPY build/agent-os-rootfs /

ENV AOS_VERSION=0.2.0
ENV WORKSPACE=/workspace

WORKDIR /workspace
CMD ["/bin/bash", "--login"]
