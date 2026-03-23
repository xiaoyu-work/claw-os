FROM scratch

COPY build/claw-os-rootfs /

ENV COS_VERSION=0.1.0
ENV NODE_MAJOR=24
ENV DEN=/den
ENV COS_BROWSER_URL=http://localhost:3000

WORKDIR /den
ENTRYPOINT ["/usr/local/bin/cos-init"]
CMD ["/bin/bash", "--login"]
