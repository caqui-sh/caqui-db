FROM codercom/code-server:latest
USER root
RUN apt-get update 
RUN apt-get install -y build-essential libsqlite3-dev sqlite3 gcc pkg-config
RUN apt-get install -y curl
RUN apt-get install unzip
ENV HOME="/home/coder"

# Install Node and Gemini CLI
RUN apt-get install -y gpg
RUN mkdir -p /etc/apt/keyrings && \
    curl -fsSL https://deb.nodesource.com/gpgkey/nodesource-repo.gpg.key | gpg --dearmor -o /etc/apt/keyrings/nodesource.gpg && \
    echo "deb [signed-by=/etc/apt/keyrings/nodesource.gpg] https://deb.nodesource.com/node_20.x nodistro main" | tee /etc/apt/sources.list.d/nodesource.list
RUN apt-get update
RUN apt-get install -y nodejs
RUN npm install -g @google/gemini-cli
RUN rm -rf /home/coder/.cache

# Install Rust toolchain as root and configure PATH identically to Deno
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
RUN echo "export PATH=\"$HOME/.cargo/bin:\$PATH\"" >> $HOME/.bashrc

# Fix permissions so the coder user can write to the downloaded registries and toolchain folders
RUN chown -R coder:coder $HOME/.cargo $HOME/.rustup $HOME/.bashrc

USER coder
