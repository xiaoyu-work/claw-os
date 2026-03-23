FROM scratch

COPY build/claw-os-rootfs /

ENV COS_VERSION=0.1.0
ENV NODE_MAJOR=24
ENV WORKSPACE=/workspace
ENV COS_BROWSER_URL=http://localhost:3000

WORKDIR /workspace
ENTRYPOINT ["/usr/local/bin/cos-init"]
CMD ["/bin/bash", "--login"]
