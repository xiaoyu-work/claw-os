FROM scratch

COPY build/agent-os-rootfs /

ENV AOS_VERSION=0.3.0
ENV WORKSPACE=/workspace
ENV JINA_READER_URL=http://localhost:3000

WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/aos-init"]
CMD ["/bin/bash", "--login"]
