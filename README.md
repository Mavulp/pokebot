# pokebot

```
pokebot 0.1.1
Jokler <jokler@protonmail.com>

USAGE:
    pokebot [FLAGS] [OPTIONS] [config_path]

FLAGS:
    -h, --help       Prints help information
    -l, --local      Run locally in text mode
    -V, --version    Prints version information
    -v, --verbose    Print the content of all packets

OPTIONS:
    -a, --address <address>                     The address of the server to connect to
    -g, --generate-identities <gen_id_count>    Generate 'count' identities
    -d, --master_channel <master_channel>       The channel the master bot should connect to

ARGS:
    <config_path>    Configuration file [default: config.toml]
```
# Usage

 1. Poke the main bot.
 2. Once the secondary bot joins your channel, type !help for a list of commands.
 
 **Chat commands:**
 ```
    add       Adds url to playlist
    clear     Clears the playback queue
    help      Prints this message or the help of the given subcommand(s)
    leave     Leaves the channel
    next      Switches to the next queue entry
    pause     Pauses audio playback
    play      Starts audio playback
    search    Adds the first video found on YouTube
    seek      Seeks by a specified amount
    stop      Stops audio playback
    volume    Changes the volume to the specified value
 ```
