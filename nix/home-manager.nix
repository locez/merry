{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib)
    filterAttrs
    isAttrs
    isList
    literalExpression
    mapAttrs
    mkEnableOption
    mkIf
    mkOption
    types
    ;

  cfg = config.programs.merry;
  tomlFormat = pkgs.formats.toml { };

  # TOML has no null value. Optional options are represented as `null` in Nix
  # and omitted from the generated document.
  stripNulls =
    value:
    if isAttrs value then
      mapAttrs (_: stripNulls) (filterAttrs (_: item: item != null) value)
    else if isList value then
      builtins.filter (item: item != null) (map stripNulls value)
    else
      value;

  providerType = types.submodule {
    options = {
      displayName = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Human-readable provider name shown in Merry. Defaults to the alias.";
      };

      defaultModel = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Default model for this provider.";
      };

      type = mkOption {
        type = types.enum [
          "openai-compatible"
          "anthropic"
        ];
        default = "openai-compatible";
        description = "Provider protocol family.";
      };

      protocol = mkOption {
        type = types.nullOr (
          types.enum [
            "responses"
            "chat_completions"
          ]
        );
        default = null;
        description = "OpenAI-compatible wire protocol. Omit to use `responses`.";
      };

      baseUrl = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Provider API base URL.";
      };

      apiVersion = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Anthropic API version header value.";
      };

      defaultMaxOutputTokens = mkOption {
        type = types.nullOr types.ints.positive;
        default = null;
        description = "Anthropic default maximum output token count.";
      };

      reasoningEffort = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "Provider reasoning effort preset or custom value.";
      };

      serviceTier = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = "OpenAI service tier request value.";
      };

      apiKeyFile = mkOption {
        type = types.nullOr types.str;
        default = null;
        description = ''
          Runtime path to a file containing the API key. Relative paths are
          resolved under `$XDG_DATA_HOME/merry/providers`; use an absolute
          runtime path (for example an agenix or sops-nix secret) to keep the
          key out of the Nix store. Set this with `lib.mkForce` when the
          module-managed providers file is shared with other configuration.
        '';
      };
    };
  };

  providerConfig = {
    version = 1;
    providers = mapAttrs (_: provider: {
      display_name = provider.displayName;
      default_model = provider.defaultModel;
      reasoning_effort = provider.reasoningEffort;
      service_tier = provider.serviceTier;
      type = provider.type;
      protocol = provider.protocol;
      base_url = provider.baseUrl;
      api_version = provider.apiVersion;
      default_max_output_tokens = provider.defaultMaxOutputTokens;
      api_key_file = provider.apiKeyFile;
    }) cfg.providers;
  };

  providerAliasPattern = "[a-z][a-z0-9]*(-[a-z0-9]+)*";
  reservedAliases = [
    "default"
    "managed"
    "retry"
  ];

  providerAssertions = lib.flatten (
    lib.mapAttrsToList (alias: provider: [
      {
        assertion = builtins.match providerAliasPattern alias != null;
        message = "programs.merry.providers.${alias}: provider aliases must start with a lowercase letter and contain only lowercase letters, digits, or single hyphens.";
      }
      {
        assertion = builtins.stringLength alias <= 64;
        message = "programs.merry.providers.${alias}: provider aliases must be at most 64 bytes.";
      }
      {
        assertion = !(builtins.elem alias reservedAliases);
        message = "programs.merry.providers.${alias}: provider aliases may not be named \"default\", \"managed\", or \"retry\".";
      }
      {
        assertion = provider.apiKeyFile != null;
        message = "programs.merry.providers.${alias}.apiKeyFile must be set.";
      }
      {
        assertion = provider.protocol == null || provider.type == "openai-compatible";
        message = "programs.merry.providers.${alias}.protocol is only valid for openai-compatible providers.";
      }
      {
        assertion = provider.apiVersion == null || provider.type == "anthropic";
        message = "programs.merry.providers.${alias}.apiVersion is only valid for anthropic providers.";
      }
      {
        assertion = provider.defaultMaxOutputTokens == null || provider.type == "anthropic";
        message = "programs.merry.providers.${alias}.defaultMaxOutputTokens is only valid for anthropic providers.";
      }
    ]) cfg.providers
  );
in
{
  options.programs.merry = {
    enable = mkEnableOption "Merry terminal coding agent";

    package = mkOption {
      type = types.package;
      default = if pkgs ? merry then pkgs.merry else pkgs.callPackage ./package.nix { };
      defaultText = literalExpression "pkgs.callPackage ./package.nix { }";
      description = "Merry package to install.";
    };

    settings = mkOption {
      type = tomlFormat.type;
      default = { };
      description = ''
        Contents of `$XDG_CONFIG_HOME/merry/config.toml`. See
        `examples/config.toml` in the Merry repository for the full schema.
        Leaving this as `{}` leaves the file unmanaged.
      '';
      example = literalExpression ''
        {
          cli = {
            permission_mode = "auto";
            auto_fallback = "ask";
          };
          skills.roots = [ "skills" ];
          tui.theme.preset = "sakura";
        }
      '';
    };

    providers = mkOption {
      type = types.attrsOf providerType;
      default = { };
      description = ''
        Declarative provider registry written to
        `$XDG_DATA_HOME/merry/providers/providers.toml`. Provider credentials
        remain in runtime files referenced by `apiKeyFile` and are never
        embedded in the Nix store. Leaving this as `{}` leaves the registry
        unmanaged.
      '';
      example = literalExpression ''
        {
          openai = {
            displayName = "OpenAI";
            defaultModel = "gpt-5";
            baseUrl = "https://api.openai.com/v1";
            apiKeyFile = "/run/agenix/merry-openai-key";
          };
        }
      '';
    };
  };

  config = mkIf cfg.enable {
    assertions = providerAssertions;

    home.packages = [ cfg.package ];

    xdg.configFile = lib.optionalAttrs (cfg.settings != { }) {
      "merry/config.toml".source = tomlFormat.generate "merry-config.toml" (stripNulls cfg.settings);
    };

    xdg.dataFile = lib.optionalAttrs (cfg.providers != { }) {
      "merry/providers/providers.toml".source = tomlFormat.generate "merry-providers.toml" (
        stripNulls providerConfig
      );
    };
  };
}
