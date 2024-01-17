# Telearia2

Manage aria2 with telegram bot.

<img width="600" alt="image" src="https://github.com/ihciah/telearia2/assets/1707505/7c2fa20c-16e8-40cb-a840-71a7a91b3726">

## How to Deploy
1. Copy `config_example.toml` and edit it with your bot token, aria2 json-rpc address, token, admin ids and other settings.
2. Copy `docker-compose.yml` and run `docker-compose up -d`.

## Features
1. Add tasks via torrent file, http(s) link or magnet link
2. Monitor download task progress in real time
3. Basic operations on tasks (pause, delete, etc.)
