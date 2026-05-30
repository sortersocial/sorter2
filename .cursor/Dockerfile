# System image for Cursor Cloud Agents (sorter2).
# Do not COPY the project — Cursor checks out the repo separately.
FROM ubuntu:24.04

ENV DEBIAN_FRONTEND=noninteractive
ENV CXX=g++
ENV PATH="/root/.local/bin:${PATH}"

RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    g++ \
    pkg-config \
    libssl-dev \
    curl \
    ca-certificates \
    git \
    sudo \
    # Playwright Chromium runtime libraries (install-deps subset)
    libasound2t64 \
    libatk-bridge2.0-0t64 \
    libatk1.0-0t64 \
    libatspi2.0-0t64 \
    libcairo2 \
    libcups2t64 \
    libdbus-1-3 \
    libdrm2 \
    libgbm1 \
    libglib2.0-0t64 \
    libnspr4 \
    libnss3 \
    libpango-1.0-0 \
    libx11-6 \
    libxcb1 \
    libxcomposite1 \
    libxdamage1 \
    libxext6 \
    libxfixes3 \
    libxkbcommon0 \
    libxrandr2 \
    fonts-liberation \
    && rm -rf /var/lib/apt/lists/*

# Clojure CLI (matches CI)
RUN curl -fsSL https://download.clojure.org/install/linux-install-1.12.0.1530.sh | bash
