// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn, Meta, MetaNameValue};

/// Attribute macro for registering test functions.
///
/// # Example
/// ```ignore
/// #[quake_test(group = "probe", name = "connectivity")]
/// fn connectivity_test<'a>(
///     testnet: &'a Testnet,
///     factory: &'a RpcClientFactory,
///     _params: &'a TestParams,
/// ) -> TestResult<'a> {
///     Box::pin(async move {
///         // test implementation
///     })
/// }
///
/// // Disabled test (will not run)
/// #[quake_test(group = "probe", name = "flaky", disabled = true)]
/// fn flaky_test<'a>(
///     testnet: &'a Testnet,
///     factory: &'a RpcClientFactory,
///     _params: &'a TestParams,
/// ) -> TestResult<'a> {
///     Box::pin(async move {
///         // test implementation
///     })
/// }
/// ```
#[proc_macro_attribute]
pub fn quake_test(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);

    // Parse attribute tokens to extract group, name, and disabled
    let mut group = None;
    let mut name = None;
    let mut disabled = false;

    let attr_parser = syn::parse::Parser::parse2(
        |input: syn::parse::ParseStream| {
            while !input.is_empty() {
                let meta = input.parse::<Meta>()?;
                if let Meta::NameValue(MetaNameValue { path, value, .. }) = meta {
                    if path.is_ident("group") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(lit_str),
                            ..
                        }) = value
                        {
                            group = Some(lit_str.value());
                        }
                    } else if path.is_ident("name") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(lit_str),
                            ..
                        }) = value
                        {
                            name = Some(lit_str.value());
                        }
                    } else if path.is_ident("disabled") {
                        if let syn::Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Bool(lit_bool),
                            ..
                        }) = value
                        {
                            disabled = lit_bool.value;
                        }
                    }
                }

                // Handle comma separator
                if input.peek(syn::Token![,]) {
                    input.parse::<syn::Token![,]>()?;
                }
            }
            Ok(())
        },
        proc_macro2::TokenStream::from(attr),
    );

    if let Err(e) = attr_parser {
        return e.to_compile_error().into();
    }

    let group = group.expect("quake_test macro requires 'group' attribute");
    let name = name.expect("quake_test macro requires 'name' attribute");

    let fn_name = &input_fn.sig.ident;

    // Generate a unique constant name to enforce uniqueness at compile time
    // If two tests have the same group+name, this will cause a duplicate const error
    let uniqueness_const = format!(
        "_QUAKE_TEST_REGISTRATION_{}_{}",
        group.to_uppercase().replace("-", "_"),
        name.to_uppercase().replace("-", "_")
    );
    let uniqueness_ident = syn::Ident::new(&uniqueness_const, proc_macro2::Span::call_site());

    // Generate the registration code
    let expanded = quote! {
        #input_fn

        // Compile-time uniqueness check: this const will conflict if group+name is duplicated
        #[allow(non_upper_case_globals)]
        const #uniqueness_ident: () = ();

        ::inventory::submit! {
            crate::tests::TestRegistration {
                group: #group,
                name: #name,
                test_fn: #fn_name,
                disabled: #disabled,
            }
        }
    };

    TokenStream::from(expanded)
}
