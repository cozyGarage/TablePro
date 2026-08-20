use tablepro_core::AuthMode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct AuthFormState {
    pub(super) file_based: bool,
    pub(super) supports_integrated: bool,
    pub(super) supports_local_socket: bool,
    pub(super) selected: AuthMode,
}

impl AuthFormState {
    pub(super) fn mode(self) -> AuthMode {
        if self.shows_method() {
            self.selected
        } else {
            AuthMode::Password
        }
    }

    pub(super) fn shows_method(self) -> bool {
        !self.file_based && self.supports_integrated
    }

    pub(super) fn shows_credentials(self) -> bool {
        !self.file_based && self.mode() == AuthMode::Password
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EndpointFormState {
    pub(super) file_based: bool,
    pub(super) supports_local_socket: bool,
    pub(super) socket_selected: bool,
}

impl EndpointFormState {
    pub(super) fn shows_endpoint_choice(self) -> bool {
        !self.file_based && self.supports_local_socket
    }

    pub(super) fn uses_local_socket(self) -> bool {
        self.shows_endpoint_choice() && self.socket_selected
    }

    pub(super) fn shows_network(self) -> bool {
        !self.file_based && !self.uses_local_socket()
    }

    pub(super) fn shows_port(self) -> bool {
        !self.file_based
    }

    pub(super) fn shows_socket_rows(self) -> bool {
        self.uses_local_socket()
    }
}

pub(super) fn socket_directory_is_valid(directory: &str) -> bool {
    std::path::Path::new(directory).is_absolute()
}

pub(super) fn resolved_socket_path(directory: &str, port: u16) -> std::path::PathBuf {
    std::path::Path::new(directory).join(format!(".s.PGSQL.{port}"))
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_form_state_drives_mode_and_visibility() {
        let cases = [
            (AuthFormState::default(), AuthMode::Password, false, true),
            (
                AuthFormState {
                    file_based: false,
                    supports_integrated: true,
                    supports_local_socket: false,
                    selected: AuthMode::Password,
                },
                AuthMode::Password,
                true,
                true,
            ),
            (
                AuthFormState {
                    file_based: false,
                    supports_integrated: true,
                    supports_local_socket: false,
                    selected: AuthMode::Kerberos,
                },
                AuthMode::Kerberos,
                true,
                false,
            ),
            (
                AuthFormState {
                    file_based: false,
                    supports_integrated: false,
                    supports_local_socket: false,
                    selected: AuthMode::Kerberos,
                },
                AuthMode::Password,
                false,
                true,
            ),
            (
                AuthFormState {
                    file_based: true,
                    supports_integrated: true,
                    supports_local_socket: false,
                    selected: AuthMode::Kerberos,
                },
                AuthMode::Password,
                false,
                false,
            ),
        ];
        for (state, mode, method, credentials) in cases {
            assert_eq!(state.mode(), mode, "{state:?}");
            assert_eq!(state.shows_method(), method, "{state:?}");
            assert_eq!(state.shows_credentials(), credentials, "{state:?}");
        }
    }

    #[test]
    fn a_file_based_driver_never_offers_an_endpoint_choice() {
        let state = EndpointFormState {
            file_based: true,
            supports_local_socket: false,
            socket_selected: true,
        };
        assert!(!state.shows_endpoint_choice());
        assert!(!state.uses_local_socket());
        assert!(!state.shows_socket_rows());
        assert!(!state.shows_network());
        assert!(!state.shows_port());
    }

    #[test]
    fn a_driver_without_socket_support_stays_on_the_network_form() {
        let state = EndpointFormState {
            file_based: false,
            supports_local_socket: false,
            socket_selected: true,
        };
        assert!(!state.shows_endpoint_choice());
        assert!(!state.uses_local_socket());
        assert!(state.shows_network());
        assert!(state.shows_port());
        assert!(!state.shows_socket_rows());
    }

    #[test]
    fn selecting_the_socket_endpoint_hides_every_network_row() {
        let state = EndpointFormState {
            file_based: false,
            supports_local_socket: true,
            socket_selected: true,
        };
        assert!(state.shows_endpoint_choice());
        assert!(state.uses_local_socket());
        assert!(state.shows_socket_rows());
        assert!(!state.shows_network());
        assert!(state.shows_port());
    }

    #[test]
    fn returning_to_the_network_endpoint_restores_the_network_rows() {
        let state = EndpointFormState {
            file_based: false,
            supports_local_socket: true,
            socket_selected: false,
        };
        assert!(state.shows_endpoint_choice());
        assert!(!state.uses_local_socket());
        assert!(!state.shows_socket_rows());
        assert!(state.shows_network());
    }

    #[test]
    fn a_socket_directory_must_be_absolute() {
        assert!(socket_directory_is_valid("/run/postgresql"));
        assert!(socket_directory_is_valid("/tmp"));
        assert!(!socket_directory_is_valid("run/postgresql"));
        assert!(!socket_directory_is_valid(""));
        assert!(!socket_directory_is_valid("~/sockets"));
    }

    #[test]
    fn the_resolved_socket_follows_the_directory_and_port() {
        assert_eq!(
            resolved_socket_path("/run/postgresql", 5432),
            std::path::PathBuf::from("/run/postgresql/.s.PGSQL.5432")
        );
        assert_eq!(
            resolved_socket_path("/tmp", 6543),
            std::path::PathBuf::from("/tmp/.s.PGSQL.6543")
        );
    }
}
