FROM ubuntu:24.04

RUN apt update -qqy && \
    apt upgrade -qqy && \
    apt install -qqy --no-install-recommends \
    #
    libicu74 tzdata zip unzip curl \
    python3 python3-pip python3-boto3 \
    ansible \
    #
    && apt clean -qqy && \
    #
    curl -Ls "https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip" -o "awscliv2.zip" && \
    unzip awscliv2.zip && \
    ./aws/install -i /usr/local/aws-cli -b /usr/local/bin && \
    rm -rf awscliv2.zip aws && \
    #
    curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash && \
    curl -sLO "https://dl.k8s.io/release/$(curl -L -s https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl" && \
    curl -sLO "https://dl.k8s.io/release/$(curl -L -s https://dl.k8s.io/release/stable.txt)/bin/linux/amd64/kubectl.sha256" && \
    echo "$(cat kubectl.sha256)  kubectl" | sha256sum --check && \
    install -o root -g root -m 0755 kubectl /usr/local/bin/kubectl && \
    rm kubectl kubectl.sha256

USER ubuntu
