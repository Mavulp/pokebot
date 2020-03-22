# pokebot

```
pokebot 0.2.0
Jokler <jokler@protonmail.com>

USAGE:
    pokebot [FLAGS] [OPTIONS] [config_path]

FLAGS:
    -h, --help       Prints help information
    -l, --local      Run locally in text mode
    -V, --version    Prints version information
    -v, --verbose    Print the content of all packets

OPTIONS:
    -a, --address <address>                         The address of the server to connect to
    -g, --generate-identities <gen_id_count>        Generate 'count' identities
    -d, --master_channel <master_channel>           The channel the master bot should connect to
    -w, --increase-security-level <wanted_level>    Increases the security level of all identities in the config file

ARGS:
    <config_path>    Configuration file [default: config.toml]
```
## Usage

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

## Compiling

1. Make sure the following are installed
    * cargo + rustc 1.42 or later
    * `gstreamer` development libraries which should be `libgstreamer-dev` and `libgstreamer-plugins-base-dev`

2. Clone the source with `git`:
    ```sh
    $ git clone https://github.com/Mavulp/pokebot.git
    $ cd pokebot
    ```

3. Building the binary
    ```sh
    $ cargo build --release
    ```

    This creates the binary under `target/release/`.
