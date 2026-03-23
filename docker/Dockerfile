FROM scratch

COPY build/claw-os-rootfs /

ENV COS_VERSION=0.3.0
ENV WORKSPACE=/workspace
ENV JINA_READER_URL=http://localhost:3000

WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/cos-init"]
CMD ["/bin/bash", "--login"]
